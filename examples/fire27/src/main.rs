// SPDX-License-Identifier: BSD-3-Clause
//! M5Stack Fire27 (ESP32) BSP example — display init, I2C scan, button loop.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDeviceWithConfig;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{AnyPin, Input, InputConfig, Level, Output, OutputConfig, Pull},
    i2c::master::{BusTimeout, Config as I2cConfig, I2c},
    interrupt::software::SoftwareInterruptControl,
    ram,
    spi::master::{Config as SpiConfig, Spi},
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_println as _;
use esp_sync::RawMutex;
use lcd_async::{
    Builder,
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
};
use log::info;
use m5stack_core::io::shared_i2c::SharedI2cBus;
use static_cell::make_static;

#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    info!("custom_halt — resetting");
    esp_hal::system::software_reset();
    #[allow(clippy::empty_loop)]
    loop {}
}

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(
        esp_hal::Config::default().with_cpu_clock(CpuClock::max()),
    );
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

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
    let _display = Builder::new(ILI9342CRgb565, di)
        .invert_colors(ColorInversion::Inverted)
        .color_order(ColorOrder::Bgr)
        .display_size(320, 240)
        .reset_pin(rst)
        .init(&mut delay)
        .await
        .expect("Display init failed");

    bl.set_high();
    info!("Display initialized, entering button loop");

    // --- Button loop ---
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
        Timer::after(Duration::from_millis(100)).await;
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
