// SPDX-License-Identifier: MIT OR Apache-2.0
//! I2C bus scan — enumerate `0x08..=0x77` and show the result on the LCD.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use common::{STRIP_BYTES, draw_panel, i2c_scan};
use demos::board::{self, NAME};
use demos::shim;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use static_cell::make_static;

// Panic handler + log/console transport come from the BSP (the panic-handler +
// console-serial features); the app descriptor is the one line the binary keeps.
m5stack_core::app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();
    let board = board::Board::split(p);
    shim::init_heaps_default(board.psram);
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);
    #[cfg(feature = "fire27")]
    let _console =
        shim::init_console(spawner, board::console_serial(board.uart0, board.uart0_rx, board.uart0_tx));
    #[cfg(feature = "cores3")]
    let _console = shim::init_console(spawner, board::console_serial(board.usb_device));

    let (mut display, i2c) = board::init_display(board.spi2, board.i2c0).await;

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);

    loop {
        let found = i2c_scan(&mut *i2c.lock().await).await;
        log::info!("I2C scan 0x08..0x77: {} device(s)", found.len());
        for addr in &found {
            log::info!("  Found device at 0x{:02x}", addr);
        }

        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::format!("{} device(s)", found.len()));
        for addr in &found {
            lines.push(alloc::format!("0x{:02x}", addr));
        }
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_panel(&mut display, &mut strip_buf[..], NAME, "I2C scan", &refs).await;

        Timer::after(Duration::from_secs(2)).await;
    }
}
