// SPDX-License-Identifier: MIT OR Apache-2.0
//! M5Stack CoreS3 (ESP32-S3) BSP example — display demo, I2C scan, touch loop.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDeviceWithConfig;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    draw_target::DrawTarget,
    mono_font::{MonoTextStyle, ascii::FONT_9X18_BOLD},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, PrimitiveStyleBuilder, Rectangle, Triangle},
    text::Text,
};
use panic_halt as _;
esp_bootloader_esp_idf::esp_app_desc!();
use embassy_net::StackResources;
use embedded_hal::digital::OutputPin;
use esp_hal::rng::Rng;
use esp_hal::{
    gpio::{AnyPin, Level, Output, OutputConfig},
    i2c::master::{BusTimeout, Config as I2cConfig, I2c},
    interrupt::software::SoftwareInterruptControl,
    ram,
    spi::master::{Config as SpiConfig, Spi},
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_sync::RawMutex;
use lcd_async::{
    Builder, Display,
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
    raw_framebuf::RawFrameBuf,
};
use m5stack_core::driver::aw9523b::{Aw9523bDriver, Aw9523bResources};
use m5stack_core::driver::axp2101::Axp2101Driver;
use m5stack_core::driver::ft6336u;
#[cfg(feature = "coex")]
use m5stack_core::driver::radio::ble::BleRadio;
use m5stack_core::driver::radio::wifi::{self, AuthenticationMethod, IpSetup, StaCredentials};
use m5stack_core::driver::sk6812::{Rgb, Sk6812Driver};
use m5stack_core::io::shared_i2c::SharedI2cBus;
use rtt_target::rprintln;
use static_cell::make_static;

#[cfg(feature = "coex")]
mod ble;

const W: usize = 320;
const H: usize = 240;
const STRIP_H: usize = 40;
const STRIP_BYTES: usize = W * STRIP_H * 2;

/// WiFi credentials, supplied at build time. When `WIFI_SSID` is unset the demo
/// skips WiFi and just runs the display:
/// `WIFI_SSID=ssid WIFI_PASSWORD=pw cargo +esp run --release -p cores3 --target xtensa-esp32s3-none-elf`
const WIFI_SSID: Option<&str> = option_env!("WIFI_SSID");
const WIFI_PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) {
    // CRITICAL: esp_hal::init() MUST come before rtt_init_print!()
    let peripherals = esp_hal::init(esp_hal::Config::default());
    rtt_target::rtt_init_print!();
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
    // WiFi keeps its RX/TX buffers in internal SRAM. NOTE: esp-alloc's global
    // heap holds at most 3 regions — this internal heap, the reclaimed region
    // above, and the PSRAM region (registered below) are exactly 3, so do NOT
    // add a 4th `heap_allocator!`. Coex (WiFi + BLE) needs more controller heap,
    // so it gets a larger region.
    #[cfg(not(feature = "coex"))]
    esp_alloc::heap_allocator!(size: 64 * 1024);
    #[cfg(feature = "coex")]
    esp_alloc::heap_allocator!(size: 96 * 1024);

    // --- PSRAM heap (CoreS3 carries ~8 MB SPI PSRAM) ---
    // Registers PSRAM as an external heap region, then shows an application
    // explicitly placing a large buffer there via `ExternalMemory`. The S3 can
    // also DMA from PSRAM (subject to cache/alignment rules), so a full
    // 320x240x2 framebuffer could live here instead of the strip workaround.
    let psram_free = m5stack_core::mem::init_psram_heap(peripherals.PSRAM);
    {
        // Checked PSRAM allocation: `psram_vec` bounds `T: PsramSafe`, so an
        // atomic-bearing element type would be a compile error here.
        let mut scratch = m5stack_core::mem::psram_vec::<u8>(256 * 1024);
        scratch.resize(256 * 1024, 0xa5);
        rprintln!(
            "PSRAM: {} KiB free, 256 KiB scratch @ {:p}",
            psram_free / 1024,
            scratch.as_ptr(),
        );
    }

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

    // --- WiFi (STA + DHCP) ---
    // The BSP brings up the station and the embassy-net stack; the app supplies a
    // seed (here from the RNG; real apps derive it from their TRNG) and the
    // `StackResources`, then spawns the runner task. `Stack` is `Copy`, so we
    // keep a handle for the on-screen IP readout.
    let mut wifi_stack: Option<embassy_net::Stack<'static>> = None;
    if let Some(ssid) = WIFI_SSID {
        let rng = Rng::new();
        let seed = ((rng.random() as u64) << 32) | rng.random() as u64;
        let resources = make_static!(StackResources::<3>::new());
        let creds = StaCredentials {
            ssid,
            password: WIFI_PASSWORD.unwrap_or(""),
            auth: AuthenticationMethod::Wpa2Personal,
        };
        match wifi::Wifi::new(peripherals.WIFI)
            .and_then(|w| w.into_sta(creds, IpSetup::Dhcp, seed, resources))
        {
            Ok((stack, control, runner)) => {
                wifi_stack = Some(stack);
                spawner.spawn(wifi::wifi_task(runner).unwrap());
                spawner.spawn(net_demo(stack, control).unwrap());
            }
            Err(e) => rprintln!("WiFi init failed: {:?}", e),
        }
    } else {
        rprintln!("WiFi disabled (set WIFI_SSID/WIFI_PASSWORD to enable)");
    }

    // --- BLE peer-MAC scanner (coexistence) ---
    #[cfg(feature = "coex")]
    match BleRadio::new(peripherals.BT) {
        Ok(ble) => {
            spawner.spawn(ble::ble_scan_task(ble).unwrap());
        }
        Err(e) => rprintln!("BLE init failed: {:?}", e),
    }

    // --- I2C ---
    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default()
            .with_frequency(Rate::from_khz(400))
            .with_timeout(BusTimeout::BusCycles(20)),
    )
    .expect("I2C0 init failed")
    .with_sda(AnyPin::from(peripherals.GPIO12))
    .with_scl(AnyPin::from(peripherals.GPIO11))
    .into_async();

    let i2c_bus: &'static SharedI2cBus = make_static!(SharedI2cBus::new(i2c));
    i2c_scan(i2c_bus).await;

    // --- AW9523B: LCD + touch reset ---
    let mut aw = Aw9523bDriver::new(Aw9523bResources { i2c: i2c_bus });
    if let Err(e) = aw.init().await {
        rprintln!("AW9523B init failed: {:?}", e);
    }
    if let Err(e) = aw.lcd_rst_pulse().await {
        rprintln!("AW9523B LCD RST failed: {:?}", e);
    }
    if let Err(e) = aw.touch_rst_pulse().await {
        rprintln!("AW9523B TOUCH RST failed: {:?}", e);
    }

    // --- AXP2101: backlight ---
    let mut axp = Axp2101Driver::new(i2c_bus, 0x34);
    if let Err(e) = axp.set_dldo1(true, 3300).await {
        rprintln!("AXP2101 backlight enable failed: {:?}", e);
    }
    // CoreS3 manages the battery (incl. the M5GO bottom's cell) via this AXP2101,
    // not the bottom's IP5306. Enable the VBAT ADC so we can read the voltage.
    if let Err(e) = axp.enable_battery_adc().await {
        rprintln!("AXP2101 battery ADC enable failed: {:?}", e);
    }

    // --- SPI display (GPIO35 = DC, no RST pin — handled by AW9523B) ---
    let spi_config = SpiConfig::default()
        .with_frequency(Rate::from_khz(400))
        .with_mode(esp_hal::spi::Mode::_0);
    let spi = Spi::new(peripherals.SPI2, spi_config.clone())
        .expect("SPI2 init failed")
        .with_sck(AnyPin::from(peripherals.GPIO36))
        .with_mosi(AnyPin::from(peripherals.GPIO37))
        .into_async();

    let display_cs = Output::new(
        AnyPin::from(peripherals.GPIO3),
        Level::Low,
        OutputConfig::default(),
    );

    let shared_spi = make_static!(Mutex::<RawMutex, _>::new(spi));
    let spi_device = SpiDeviceWithConfig::new(
        shared_spi,
        display_cs,
        spi_config.with_frequency(Rate::from_khz(40_000)).clone(),
    );
    // Display DC on GPIO35. The example doesn't use SD/MISO, so GPIO35 is a
    // plain output here — `Output::new` configures the pad's IO-MUX so the pin
    // actually drives (a bare GPIO-register hack leaves the pad unrouted and DC
    // never toggles, so the panel never wakes → black screen).
    let dc = Output::new(
        AnyPin::from(peripherals.GPIO35),
        Level::Low,
        OutputConfig::default(),
    );
    let di = SpiInterface::new(spi_device, dc);
    let mut delay = embassy_time::Delay;
    let mut display = Builder::new(ILI9342CRgb565, di)
        .invert_colors(ColorInversion::Inverted)
        .color_order(ColorOrder::Bgr)
        .display_size(320, 240)
        .init(&mut delay)
        .await
        .expect("Display init failed");

    rprintln!("Display initialized");

    // Strip framebuffer in a static internal-RAM buffer (the SPI DMA/FIFO
    // source), shared by the splash and the status loop — allocated once, never
    // leaked per frame.
    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);

    draw_demo(
        &mut display,
        &mut strip_buf[..],
        "CoreS3",
        &["coex smoke test"],
    )
    .await;
    rprintln!("Demo drawn, entering status loop");

    // --- M5GO Battery Bottom: SK6812 LED bars on M-Bus pin 23 = GPIO13 on the
    // ESP32-S3 CoreS3 (a *different* GPIO than the Fire's pin-23/GPIO15). The
    // battery is read above via the AXP2101 — CoreS3's own PMIC manages the
    // cell, so the bottom's IP5306 (used on the PMIC-less Basic Core / Fire) is
    // not the battery path here. Best-effort: LED writes go nowhere if absent.
    let mut leds = Sk6812Driver::new(peripherals.RMT, AnyPin::from(peripherals.GPIO13))
        .inspect_err(|e| rprintln!("SK6812 init failed: {:?}", e))
        .ok();
    let mut led_step: u8 = 0;

    // --- M5GO bottom 5V output: power the SK6812 LED bars ---
    // The bottom's LEDs are fed from the CoreS3 M-Bus 5V rail, which is the
    // SY7088 boost + load switch gated by the AW9523 (BOOST_EN=P1_7, BUS_OUT_EN
    // =P0_1, both active-HIGH — verified vs M5Unified). M5Unified only refuses to
    // enable it when there's NO battery AND USB is present (shared-VBUS contention),
    // so we replicate that guard: enable when a battery is present *or* USB is
    // absent. (The A014 bottom can't sustain CoreS3 on battery — it powers down on
    // unplug — so in practice this runs on USB with the bottom's battery present.)
    let vbus = axp.vbus_present().await.unwrap_or(true);
    let mv = axp.battery_voltage_mv().await.unwrap_or(0);
    let battery_present = mv > 3300;
    let bus_5v_on = if battery_present || !vbus {
        match aw.enable_bus_5v().await {
            Ok(()) => {
                rprintln!(
                    "M-Bus 5V enabled (BOOST_EN+BUS_OUT_EN); batt={}mV vbus={}",
                    mv,
                    vbus
                );
                true
            }
            Err(e) => {
                rprintln!("enable_bus_5v failed: {:?}", e);
                false
            }
        }
    } else {
        rprintln!("M-Bus 5V NOT enabled — no battery while on USB (contention guard)");
        false
    };
    {
        let l1 = alloc::format!("5V bus: {}", if bus_5v_on { "ON" } else { "OFF" });
        let l2 = alloc::format!("batt {}mV vbus={}", mv, if vbus { "Y" } else { "N" });
        draw_status(
            &mut display,
            &mut strip_buf[..],
            &[
                "M5GO LED test",
                "",
                &l1,
                &l2,
                "",
                "LEDs should cycle",
                "on G13",
            ],
        )
        .await;
    }
    Timer::after(Duration::from_millis(1500)).await;

    // --- Status loop: show the DHCP IP and discovered BLE peer MACs ---
    loop {
        match ft6336u::read_touch(i2c_bus).await {
            Ok(Some((x, y))) => rprintln!("Touch: x={} y={}", x, y),
            Ok(None) => {}
            Err(e) => rprintln!("Touch read error: {:?}", e),
        }

        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::string::String::from("CoreS3 coex"));
        match wifi_stack.and_then(|s| s.config_v4()) {
            Some(cfg) => lines.push(alloc::format!("IP {}", cfg.address)),
            None => lines.push(alloc::string::String::from(if wifi_stack.is_some() {
                "WiFi: connecting..."
            } else {
                "WiFi: disabled"
            })),
        }
        match (axp.battery_voltage_mv().await, axp.vbus_present().await) {
            (Ok(mv), Ok(vbus)) => lines.push(alloc::format!(
                "Batt {} mV {}",
                mv,
                if vbus { "USB" } else { "" }
            )),
            _ => lines.push(alloc::string::String::from("Batt: read err")),
        }
        #[cfg(feature = "coex")]
        {
            lines.push(alloc::string::String::from("BLE peers:"));
            for mac in ble::snapshot() {
                // Conventional MSB-first notation (raw() is little-endian).
                lines.push(alloc::format!(
                    "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    mac[5],
                    mac[4],
                    mac[3],
                    mac[2],
                    mac[1],
                    mac[0]
                ));
            }
        }
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_status(&mut display, &mut strip_buf[..], &refs).await;

        // Rotate a colour wheel across the 10 SK6812 LEDs on the bottom.
        if let Some(leds) = leds.as_mut() {
            let mut frame = [Rgb::OFF; 10];
            for (i, px) in frame.iter_mut().enumerate() {
                let c = wheel(led_step.wrapping_add((i as u8) * 25));
                // ~1/4 brightness — comfortable behind the frosted diffusers.
                *px = Rgb::new(c.r >> 2, c.g >> 2, c.b >> 2);
            }
            if let Err(e) = leds.write(&frame).await {
                rprintln!("SK6812 write failed: {:?}", e);
            }
            led_step = led_step.wrapping_add(8);
        }

        Timer::after(Duration::from_millis(500)).await;
    }
}

/// Wait for a DHCP lease, print the IP, then scan for nearby APs (rprintln).
#[embassy_executor::task]
async fn net_demo(stack: embassy_net::Stack<'static>, control: wifi::WifiControl) {
    rprintln!("WiFi: connecting + waiting for DHCP...");
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        rprintln!("WiFi: got IP {}", cfg.address);
    }
    match control.scan().await {
        Ok(aps) => {
            rprintln!("WiFi scan: {} AP(s)", aps.len());
            for ap in &aps {
                rprintln!(
                    "  {:<32} ch{:>2} {:>4} dBm",
                    ap.ssid.as_str(),
                    ap.channel,
                    ap.signal_strength
                );
            }
        }
        Err(e) => rprintln!("WiFi scan failed: {:?}", e),
    }
}

/// Render a list of text lines full-screen using the reused strip framebuffer.
async fn draw_status<DI, RST: OutputPin>(
    display: &mut Display<DI, ILI9342CRgb565, RST>,
    strip_buf: &mut [u8],
    lines: &[&str],
) where
    DI: lcd_async::interface::Interface<Word = u8>,
{
    let white = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE);
    for strip in 0..(H / STRIP_H) {
        let y_offset = (strip * STRIP_H) as i32;
        {
            let mut fb = RawFrameBuf::<Rgb565, _>::new(&mut strip_buf[..], W, STRIP_H);
            fb.clear(Rgb565::new(0, 0, 4)).ok();
            for (i, line) in lines.iter().enumerate() {
                let y = 18 + i as i32 * 18;
                Text::new(line, Point::new(8, y - y_offset), white)
                    .draw(&mut fb)
                    .ok();
            }
        }
        display
            .show_raw_data(
                0,
                (strip * STRIP_H) as u16,
                W as u16,
                STRIP_H as u16,
                strip_buf,
            )
            .await
            .ok();
    }
}

/// Draw demo scene into a DrawTarget with y_offset applied to all coordinates.
fn draw_demo_strip(fb: &mut impl DrawTarget<Color = Rgb565>, board: &str, footer: &[&str], y: i32) {
    let white = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE);
    let gray = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::CSS_LIGHT_GRAY);

    Text::new("m5stack-core BSP", Point::new(70, 30 - y), white)
        .draw(fb)
        .ok();

    let rect = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb565::YELLOW)
        .stroke_width(2)
        .fill_color(Rgb565::new(4, 8, 0))
        .build();
    Rectangle::new(Point::new(20, 50 - y), Size::new(120, 80))
        .into_styled(rect)
        .draw(fb)
        .ok();
    Text::new(board, Point::new(45, 95 - y), white)
        .draw(fb)
        .ok();

    let circle = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb565::CYAN)
        .stroke_width(2)
        .fill_color(Rgb565::new(0, 8, 4))
        .build();
    Circle::new(Point::new(170, 55 - y), 70)
        .into_styled(circle)
        .draw(fb)
        .ok();

    let green = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb565::GREEN)
        .stroke_width(2)
        .fill_color(Rgb565::new(0, 12, 0))
        .build();
    Triangle::new(
        Point::new(100, 160 - y),
        Point::new(40, 230 - y),
        Point::new(160, 230 - y),
    )
    .into_styled(green)
    .draw(fb)
    .ok();

    let red = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb565::RED)
        .stroke_width(2)
        .fill_color(Rgb565::new(8, 0, 0))
        .build();
    Triangle::new(
        Point::new(250, 150 - y),
        Point::new(190, 230 - y),
        Point::new(310, 230 - y),
    )
    .into_styled(red)
    .draw(fb)
    .ok();

    let spacing = W as i32 / (footer.len() as i32 + 1);
    for (i, label) in footer.iter().enumerate() {
        let x = spacing * (i as i32 + 1) - (label.len() as i32 * 9 / 2);
        Text::new(label, Point::new(x, 235 - y), gray).draw(fb).ok();
    }
}

/// Render demo scene to display using a caller-provided strip framebuffer
/// (must be in internal RAM — it is the SPI source).
async fn draw_demo<DI, RST: OutputPin>(
    display: &mut Display<DI, ILI9342CRgb565, RST>,
    strip_buf: &mut [u8],
    board: &str,
    footer: &[&str],
) where
    DI: lcd_async::interface::Interface<Word = u8>,
{
    for strip in 0..(H / STRIP_H) {
        let y_offset = strip * STRIP_H;
        {
            let mut fb = RawFrameBuf::<Rgb565, _>::new(&mut strip_buf[..], W, STRIP_H);
            fb.clear(Rgb565::new(0, 0, 4)).ok();
            draw_demo_strip(&mut fb, board, footer, y_offset as i32);
        }
        display
            .show_raw_data(0, y_offset as u16, W as u16, STRIP_H as u16, strip_buf)
            .await
            .ok();
    }
}

/// Map 0..=255 to a colour wheel (red → green → blue → red) at low brightness.
fn wheel(pos: u8) -> Rgb {
    let p = pos % 255;
    if p < 85 {
        Rgb::new(255 - p * 3, p * 3, 0)
    } else if p < 170 {
        let p = p - 85;
        Rgb::new(0, 255 - p * 3, p * 3)
    } else {
        let p = p - 170;
        Rgb::new(p * 3, 0, 255 - p * 3)
    }
}

async fn i2c_scan(bus: &SharedI2cBus) {
    rprintln!("I2C scan 0x08..0x77:");
    for addr in 0x08..=0x77 {
        let mut buf = [0u8; 1];
        let mut guard = bus.lock().await;
        if guard.write_read_async(addr, &[], &mut buf).await.is_ok() {
            rprintln!("  Found device at 0x{:02x}", addr);
        }
    }
}
