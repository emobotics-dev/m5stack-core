// SPDX-License-Identifier: MIT OR Apache-2.0
//! CoreS3 — display splash + touch readout (minimal, no radio).
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use cores3::{Lcd, STRIP_BYTES, draw_demo, draw_status, init_display, init_i2c};
use embassy_time::{Duration, Timer};
use esp_hal::{
    gpio::AnyPin, interrupt::software::SoftwareInterruptControl, ram, timer::timg::TimerGroup,
};
use m5stack_core::driver::ft6336u;
use panic_halt as _;
use rtt_target::rprintln;
use static_cell::make_static;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) {
    // CRITICAL: esp_hal::init() MUST come before rtt_init_print!()
    let peripherals = esp_hal::init(esp_hal::Config::default());
    rtt_target::rtt_init_print!();
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
    // NOTE: esp-alloc's global heap holds at most 3 regions — this internal heap,
    // the reclaimed region above, and the PSRAM region (below) are exactly 3, so
    // do NOT add a 4th `heap_allocator!`.
    esp_alloc::heap_allocator!(size: 64 * 1024);
    // The S3 can DMA from PSRAM, but the strip framebuffer stays in internal RAM
    // here; PSRAM is registered only to expose the external heap region.
    m5stack_core::mem::init_psram_heap(peripherals.PSRAM);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

    let i2c_bus = init_i2c(
        peripherals.I2C0,
        AnyPin::from(peripherals.GPIO12),
        AnyPin::from(peripherals.GPIO11),
    );

    let shared_spi = make_static!(None);
    let (mut display, _axp): (Lcd, _) = init_display(
        i2c_bus,
        shared_spi,
        peripherals.SPI2,
        AnyPin::from(peripherals.GPIO36),
        AnyPin::from(peripherals.GPIO37),
        AnyPin::from(peripherals.GPIO3),
        AnyPin::from(peripherals.GPIO35),
    )
    .await;
    rprintln!("Display initialized");

    // Strip framebuffer in a static internal-RAM buffer (the SPI DMA/FIFO
    // source), shared by the splash and the touch loop — allocated once, never
    // leaked per frame.
    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);
    draw_demo(
        &mut display,
        &mut strip_buf[..],
        "CoreS3",
        &["display demo"],
    )
    .await;

    loop {
        let touch = match ft6336u::read_touch(i2c_bus).await {
            Ok(Some((x, y))) => {
                rprintln!("Touch: x={} y={}", x, y);
                Some((x, y))
            }
            Ok(None) => None,
            Err(e) => {
                rprintln!("Touch read error: {:?}", e);
                None
            }
        };

        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::string::String::from("CoreS3 touch"));
        match touch {
            Some((x, y)) => lines.push(alloc::format!("x={} y={}", x, y)),
            None => lines.push(alloc::string::String::from("(no touch)")),
        }
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_status(&mut display, &mut strip_buf[..], &refs).await;
        Timer::after(Duration::from_millis(100)).await;
    }
}
