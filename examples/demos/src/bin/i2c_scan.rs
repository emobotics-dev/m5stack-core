// SPDX-License-Identifier: MIT OR Apache-2.0
//! I2C bus scan — enumerate `0x08..=0x77` and show the result on the LCD.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use common::{STRIP_BYTES, draw_demo, draw_status, i2c_scan};
use demos::board::{self, NAME};
use demos::shim;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use static_cell::make_static;

// Per-board panic handler + logger backend. Fire27: esp-backtrace over the UART
// console + esp-println. CoreS3: panic-halt (USB-Serial-JTAG, with which
// esp-backtrace/esp-println conflict — it logs over RTT; see shim::init_logger).
#[cfg(feature = "fire27")]
use esp_backtrace as _;
#[cfg(feature = "fire27")]
use esp_println as _;
#[cfg(feature = "cores3")]
use panic_halt as _;

esp_bootloader_esp_idf::esp_app_desc!();

// esp-backtrace's `custom-halt` feature calls this after the backtrace (Fire27).
#[cfg(feature = "fire27")]
#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) {
    let p = board::init();
    shim::init_logger();
    let board = board::Board::split(p);
    shim::init_heaps_default(board.psram);
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);

    let (mut display, i2c) = board::init_display(board.spi2, board.i2c0).await;

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);
    draw_demo(&mut display, &mut strip_buf[..], NAME, &["I2C scan"]).await;

    loop {
        let found = i2c_scan(&mut *i2c.lock().await).await;
        log::info!("I2C scan 0x08..0x77: {} device(s)", found.len());
        for addr in &found {
            log::info!("  Found device at 0x{:02x}", addr);
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
