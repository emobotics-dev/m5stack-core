// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fire27 — I2C bus scan.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock, gpio::AnyPin, interrupt::software::SoftwareInterruptControl, ram,
    timer::timg::TimerGroup,
};
use esp_println as _;
use fire27::{Lcd, STRIP_BYTES, draw_demo, draw_status, i2c_scan, init_display, init_i2c};
use log::info;
use static_cell::make_static;

esp_bootloader_esp_idf::esp_app_desc!();

#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    info!("custom_halt — resetting");
    loop {}
}

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // NOTE: esp-alloc's global heap holds at most 3 regions — these two plus the
    // PSRAM region (below) are exactly 3, so do NOT add a 4th `heap_allocator!`.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
    esp_alloc::heap_allocator!(size: 64 * 1024);
    // The ESP32 cannot DMA out of PSRAM; registered here only to expose the heap.
    m5stack_core::mem::init_psram_heap(peripherals.PSRAM);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

    let i2c_bus = init_i2c(
        peripherals.I2C0,
        AnyPin::from(peripherals.GPIO21),
        AnyPin::from(peripherals.GPIO22),
    );

    let shared_spi = make_static!(None);
    let (mut display, mut bl): (Lcd, _) = init_display(
        shared_spi,
        peripherals.SPI2,
        AnyPin::from(peripherals.GPIO18),
        AnyPin::from(peripherals.GPIO23),
        AnyPin::from(peripherals.GPIO19),
        AnyPin::from(peripherals.GPIO14),
        AnyPin::from(peripherals.GPIO27),
        AnyPin::from(peripherals.GPIO33),
        AnyPin::from(peripherals.GPIO32),
    )
    .await;
    bl.set_high();

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);
    draw_demo(&mut display, &mut strip_buf[..], "Fire27", &["I2C scan"]).await;

    loop {
        let found = i2c_scan(&mut *i2c_bus.lock().await).await;
        info!("I2C scan 0x08..0x77: {} device(s)", found.len());
        for addr in &found {
            info!("  Found device at 0x{:02x}", addr);
        }

        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::format!("I2C: {} device(s)", found.len()));
        for addr in &found {
            lines.push(alloc::format!("  0x{:02x}", addr));
        }
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_status(&mut display, &mut strip_buf[..], &refs).await;

        Timer::after(Duration::from_secs(2)).await;
    }
}
