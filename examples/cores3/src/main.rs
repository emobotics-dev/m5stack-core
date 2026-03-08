// SPDX-License-Identifier: BSD-3-Clause
//! M5Stack CoreS3 (ESP32-S3) BSP example — display demo, I2C scan, touch loop.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use panic_halt as _;
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
use embedded_hal::digital::{ErrorType, OutputPin};
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
use m5stack_core::io::shared_i2c::SharedI2cBus;
use rtt_target::rprintln;
use static_cell::make_static;

const W: usize = 320;
const H: usize = 240;
const STRIP_H: usize = 40;
const STRIP_BYTES: usize = W * STRIP_H * 2;

/// GPIO35 DC pin via direct register writes (GPIO35 is muxed MISO/DC on CoreS3).
const BIT: u32 = 1 << (35 - 32);

struct Gpio35Dc;

impl ErrorType for Gpio35Dc {
    type Error = core::convert::Infallible;
}

impl OutputPin for Gpio35Dc {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        unsafe {
            let gpio = &*esp_hal::peripherals::GPIO::PTR;
            gpio.out1_w1tc().write(|w| w.bits(BIT));
            gpio.enable1_w1ts().write(|w| w.bits(BIT));
        }
        Ok(())
    }
    fn set_high(&mut self) -> Result<(), Self::Error> {
        unsafe {
            let gpio = &*esp_hal::peripherals::GPIO::PTR;
            gpio.out1_w1ts().write(|w| w.bits(BIT));
            gpio.enable1_w1ts().write(|w| w.bits(BIT));
        }
        Ok(())
    }
}

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) {
    // CRITICAL: esp_hal::init() MUST come before rtt_init_print!()
    let peripherals = esp_hal::init(esp_hal::Config::default());
    rtt_target::rtt_init_print!();
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

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
    let di = SpiInterface::new(spi_device, Gpio35Dc);
    let mut delay = embassy_time::Delay;
    let mut display = Builder::new(ILI9342CRgb565, di)
        .invert_colors(ColorInversion::Inverted)
        .color_order(ColorOrder::Bgr)
        .display_size(320, 240)
        .init(&mut delay)
        .await
        .expect("Display init failed");

    rprintln!("Display initialized");

    draw_demo(&mut display, "CoreS3", &["Touch anywhere"]).await;
    rprintln!("Demo drawn, entering touch loop");

    // --- Touch loop ---
    loop {
        match ft6336u::read_touch(i2c_bus).await {
            Ok(Some((x, y))) => rprintln!("Touch: x={} y={}", x, y),
            Ok(None) => {}
            Err(e) => rprintln!("Touch read error: {:?}", e),
        }
        Timer::after(Duration::from_millis(50)).await;
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

/// Render demo scene to display using strip-based framebuffer (25 KB heap).
async fn draw_demo<DI, RST: OutputPin>(
    display: &mut Display<DI, ILI9342CRgb565, RST>,
    board: &str,
    footer: &[&str],
) where
    DI: lcd_async::interface::Interface<Word = u8>,
{
    let strip_buf = alloc::vec![0u8; STRIP_BYTES];
    let strip_buf: &'static mut [u8] = strip_buf.leak();

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
    rprintln!("I2C scan 0x08..0x77:");
    for addr in 0x08..=0x77 {
        let mut buf = [0u8; 1];
        let mut guard = bus.lock().await;
        if guard.write_read_async(addr, &[], &mut buf).await.is_ok() {
            rprintln!("  Found device at 0x{:02x}", addr);
        }
    }
}
