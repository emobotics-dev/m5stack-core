// SPDX-License-Identifier: MIT OR Apache-2.0
//! I2C bus scan — enumerate `0x08..=0x77` and show the result on the LCD.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[path = "common/mod.rs"]
mod common;

extern crate alloc;

use crate::common::board::{self, NAME};
use crate::common::helpers::{STRIP_BYTES, draw_panel, i2c_scan};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use static_cell::make_static;

// Panic handler + log/console transport come from the BSP (the panic-handler +
// console-serial features); the app descriptor is the one line the binary keeps.
m5stack_core::app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    common::boot!(spawner, board, Default);

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
