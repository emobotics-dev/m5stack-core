// SPDX-License-Identifier: MIT OR Apache-2.0
//! M5Stack Fire27 (ESP32) BSP example — display demo, I2C scan, button loop.
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
esp_bootloader_esp_idf::esp_app_desc!();
use embedded_hal::digital::OutputPin;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{AnyPin, Input, InputConfig, Level, Output, OutputConfig, Pull},
    i2c::master::{BusTimeout, Config as I2cConfig, I2c},
    interrupt::software::SoftwareInterruptControl,
    ram,
    rng::Rng,
    spi::master::{Config as SpiConfig, Spi},
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_println as _;
use esp_sync::RawMutex;
use lcd_async::{
    Builder, Display,
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
    raw_framebuf::RawFrameBuf,
};
use embassy_net::StackResources;
use log::info;
#[cfg(feature = "coex")]
use m5stack_core::driver::radio::ble::BleRadio;
use m5stack_core::driver::radio::wifi::{self, AuthenticationMethod, IpSetup, StaCredentials};
use m5stack_core::io::shared_i2c::SharedI2cBus;
use static_cell::make_static;

#[cfg(feature = "coex")]
mod ble;

const W: usize = 320;
const H: usize = 240;
const STRIP_H: usize = 40;
const STRIP_BYTES: usize = W * STRIP_H * 2;

/// WiFi credentials, supplied at build time. When `WIFI_SSID` is unset the demo
/// skips WiFi and just runs the display:
/// `WIFI_SSID=ssid WIFI_PASSWORD=pw cargo +esp run --release -p fire27`
const WIFI_SSID: Option<&str> = option_env!("WIFI_SSID");
const WIFI_PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    info!("custom_halt — resetting");
    loop {}
}

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // ESP32 DRAM is tight. Put the bulk of the WiFi/BLE heap in *reclaimed* ROM
    // RAM (a separate region) and keep the plain-DRAM `.bss` heap small, or it
    // collides with the main stack ("cannot move location counter backwards").
    // NOTE: esp-alloc's global heap holds at most 3 regions — reclaimed + this
    // internal region + the PSRAM region (below) are exactly 3, so do NOT add a
    // 4th `heap_allocator!`. (The ESP32 cannot DMA from PSRAM, so WiFi buffers
    // and the framebuffer stay in internal/reclaimed SRAM.)
    #[cfg(not(feature = "coex"))]
    {
        esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
        esp_alloc::heap_allocator!(size: 64 * 1024);
    }
    #[cfg(feature = "coex")]
    {
        esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 96 * 1024);
        esp_alloc::heap_allocator!(size: 24 * 1024);
    }

    // --- PSRAM heap (Fire27 carries ~4 MB SPI PSRAM) ---
    // Registers PSRAM as an external heap region, then shows an application
    // explicitly placing a large buffer there via `ExternalMemory` (the global
    // `alloc::vec!` keeps using internal DRAM first). Keep DMA buffers in
    // internal RAM on the ESP32 — it cannot DMA out of PSRAM.
    let psram_free = m5stack_core::mem::init_psram_heap(peripherals.PSRAM);
    {
        // Checked PSRAM allocation: `psram_vec` bounds `T: PsramSafe`, so an
        // atomic-bearing element type would be a compile error here.
        let mut scratch = m5stack_core::mem::psram_vec::<u8>(256 * 1024);
        scratch.resize(256 * 1024, 0xa5);
        info!(
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
            Err(e) => info!("WiFi init failed: {:?}", e),
        }
    } else {
        info!("WiFi disabled (set WIFI_SSID/WIFI_PASSWORD to enable)");
    }

    // --- BLE peer-MAC scanner (coexistence) ---
    #[cfg(feature = "coex")]
    match BleRadio::new(peripherals.BT) {
        Ok(ble) => {
            spawner.spawn(ble::ble_scan_task(ble).unwrap());
        }
        Err(e) => info!("BLE init failed: {:?}", e),
    }

    // --- I2C scan ---
    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default()
            .with_frequency(Rate::from_khz(400))
            .with_timeout(BusTimeout::BusCycles(20)),
    )
    .expect("I2C0 init failed")
    .with_sda(AnyPin::from(peripherals.GPIO21))
    .with_scl(AnyPin::from(peripherals.GPIO22))
    .into_async();

    let i2c_bus: &'static SharedI2cBus = make_static!(SharedI2cBus::new(i2c));
    i2c_scan(i2c_bus).await;

    // --- SPI display ---
    let spi_config = SpiConfig::default()
        .with_frequency(Rate::from_khz(400))
        .with_mode(esp_hal::spi::Mode::_0);
    let spi = Spi::new(peripherals.SPI2, spi_config.clone())
        .expect("SPI2 init failed")
        .with_sck(AnyPin::from(peripherals.GPIO18))
        .with_mosi(AnyPin::from(peripherals.GPIO23))
        .with_miso(AnyPin::from(peripherals.GPIO19))
        .into_async();

    let display_cs = Output::new(
        AnyPin::from(peripherals.GPIO14),
        Level::High,
        OutputConfig::default(),
    );
    let mut bl = Output::new(
        AnyPin::from(peripherals.GPIO32),
        Level::Low,
        OutputConfig::default(),
    );
    let dc = Output::new(
        AnyPin::from(peripherals.GPIO27),
        Level::Low,
        OutputConfig::default(),
    );
    let rst = Output::new(
        AnyPin::from(peripherals.GPIO33),
        Level::Low,
        OutputConfig::default(),
    );

    let shared_spi = make_static!(Mutex::<RawMutex, _>::new(spi));
    let spi_device = SpiDeviceWithConfig::new(
        shared_spi,
        display_cs,
        spi_config.with_frequency(Rate::from_khz(40_000)).clone(),
    );
    let di = SpiInterface::new(spi_device, dc);
    let mut delay = embassy_time::Delay;
    let mut display = Builder::new(ILI9342CRgb565, di)
        .invert_colors(ColorInversion::Inverted)
        .color_order(ColorOrder::Bgr)
        .display_size(320, 240)
        .reset_pin(rst)
        .init(&mut delay)
        .await
        .expect("Display init failed");

    bl.set_high();
    info!("Display initialized");

    // Strip framebuffer in a static internal-RAM buffer (the ESP32 cannot DMA
    // from PSRAM), shared by the splash and the status loop — allocated once,
    // never leaked per frame.
    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);

    draw_demo(&mut display, &mut strip_buf[..], "Fire27", &["coex smoke test"]).await;
    info!("Demo drawn, entering status loop");

    let btn_left = Input::new(
        AnyPin::from(peripherals.GPIO39),
        InputConfig::default().with_pull(Pull::Up),
    );
    let btn_center = Input::new(
        AnyPin::from(peripherals.GPIO38),
        InputConfig::default().with_pull(Pull::Up),
    );
    let btn_right = Input::new(
        AnyPin::from(peripherals.GPIO37),
        InputConfig::default().with_pull(Pull::Up),
    );

    // --- Status loop: show the DHCP IP and discovered BLE peer MACs ---
    loop {
        if btn_left.is_low() {
            info!("Button LEFT pressed");
        }
        if btn_center.is_low() {
            info!("Button CENTER pressed");
        }
        if btn_right.is_low() {
            info!("Button RIGHT pressed");
        }

        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::string::String::from("Fire27 coex"));
        match wifi_stack.and_then(|s| s.config_v4()) {
            Some(cfg) => lines.push(alloc::format!("IP {}", cfg.address)),
            None => lines.push(alloc::string::String::from(if wifi_stack.is_some() {
                "WiFi: connecting..."
            } else {
                "WiFi: disabled"
            })),
        }
        #[cfg(feature = "coex")]
        {
            lines.push(alloc::string::String::from("BLE peers:"));
            for mac in ble::snapshot() {
                lines.push(alloc::format!(
                    "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    mac[5], mac[4], mac[3], mac[2], mac[1], mac[0]
                ));
            }
        }
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_status(&mut display, &mut strip_buf[..], &refs).await;

        Timer::after(Duration::from_millis(500)).await;
    }
}

/// Wait for a DHCP lease, log the IP, then scan for nearby APs.
#[embassy_executor::task]
async fn net_demo(stack: embassy_net::Stack<'static>, control: wifi::WifiControl) {
    info!("WiFi: connecting + waiting for DHCP...");
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        info!("WiFi: got IP {}", cfg.address);
    }
    match control.scan().await {
        Ok(aps) => {
            info!("WiFi scan: {} AP(s)", aps.len());
            for ap in &aps {
                info!(
                    "  {:<32} ch{:>2} {:>4} dBm",
                    ap.ssid.as_str(),
                    ap.channel,
                    ap.signal_strength
                );
            }
        }
        Err(e) => info!("WiFi scan failed: {:?}", e),
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
            .show_raw_data(0, (strip * STRIP_H) as u16, W as u16, STRIP_H as u16, strip_buf)
            .await
            .ok();
    }
}

/// Draw demo scene into a DrawTarget with y_offset applied to all coordinates.
fn draw_demo_strip(fb: &mut impl DrawTarget<Color = Rgb565>, board: &str, footer: &[&str], y: i32) {
    let white = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE);
    let gray = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::CSS_LIGHT_GRAY);

    // title
    Text::new("m5stack-core BSP", Point::new(70, 30 - y), white)
        .draw(fb)
        .ok();

    // yellow rectangle with board name
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

    // cyan circle
    let circle = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb565::CYAN)
        .stroke_width(2)
        .fill_color(Rgb565::new(0, 8, 4))
        .build();
    Circle::new(Point::new(170, 55 - y), 70)
        .into_styled(circle)
        .draw(fb)
        .ok();

    // green triangle
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

    // red triangle
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

    // footer labels evenly spaced
    let spacing = W as i32 / (footer.len() as i32 + 1);
    for (i, label) in footer.iter().enumerate() {
        let x = spacing * (i as i32 + 1) - (label.len() as i32 * 9 / 2);
        Text::new(label, Point::new(x, 235 - y), gray).draw(fb).ok();
    }
}

/// Render demo scene to display using a caller-provided strip framebuffer
/// (must be in internal RAM — the ESP32 cannot DMA from PSRAM).
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

async fn i2c_scan(bus: &SharedI2cBus) {
    info!("I2C scan 0x08..0x77:");
    for addr in 0x08..=0x77 {
        let mut buf = [0u8; 1];
        let mut guard = bus.lock().await;
        if guard.write_read_async(addr, &[], &mut buf).await.is_ok() {
            info!("  Found device at 0x{:02x}", addr);
        }
    }
}
