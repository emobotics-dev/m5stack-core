// SPDX-License-Identifier: MIT OR Apache-2.0
//! DS18B20 1-Wire temperature read over RMT (HIL test for the vendored
//! `m5stack_core::driver::onewire` / `ds18b20`). **Fire27 only** — the CoreS3
//! has no equivalent demo (required-features = ["fire27"]).
//!
//! Wiring: DS18B20 sensors on **Port B (black)**, data line on **G26** (the
//! signal pin following VCC; the other Port-B pin, G36, is input-only and can't
//! drive the bidirectional bus). External 4.7k pull-up to 3V3 required.
//!
//! Run: `cargo +esp run --release -p demos --bin onewire`
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[path = "common/mod.rs"]
mod common;

extern crate alloc;

use crate::common::helpers::{STRIP_BYTES, draw_panel};
use crate::common::board::{self, NAME};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use m5stack_core::driver::ds18b20::Ds18b20Driver;
use static_cell::make_static;

// Panic handler + log/console transport come from the BSP (the panic-handler +
// console-serial features); the app descriptor is the one line the binary keeps.
m5stack_core::app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    // onewire is Fire27-only (required-features), so only the fire27 arm builds.
    common::boot!(spawner, board, Default);

    let (mut display, _i2c) = board::init_display(board.spi2, board.i2c0).await;
    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);

    log::info!("== DS18B20 1-Wire test — data on G26 (Port B / black) ==");
    let mut ds = match Ds18b20Driver::new(board.rmt, board.m5bus.gpio26) {
        Ok(d) => d,
        Err(e) => {
            log::error!("ds18b20 init failed: {:?}", e);
            loop {
                draw_panel(&mut display, &mut strip_buf[..], NAME, "1-Wire", &["init failed", "(pull-up present?)"]).await;
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    };

    loop {
        // One pass over the readings: log each sensor and build the screen lines
        // (short ROM + temperature). DS18B20 addresses are read-once and cached.
        let mut sensors: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        let header = match ds.read_all_temperatures().await {
            Ok(temps) => {
                for (addr, temp) in temps {
                    log::info!("  sensor {:#018x} = {} C", addr.0, temp);
                    sensors.push(alloc::format!("{:#018x} {:.1}C", addr.0, temp));
                }
                log::info!("-> found {} DS18B20 sensor(s)", sensors.len());
                alloc::format!("{} sensor(s)", sensors.len())
            }
            Err(e) => {
                log::error!("read error: {:?}", e);
                alloc::string::String::from("read error")
            }
        };

        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(header);
        lines.extend(sensors);
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_panel(&mut display, &mut strip_buf[..], NAME, "1-Wire", &refs).await;

        Timer::after(Duration::from_secs(2)).await;
    }
}
