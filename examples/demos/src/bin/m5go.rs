// SPDX-License-Identifier: MIT OR Apache-2.0
//! M5GO Battery Bottom: SK6812 LED bars + battery readout.
//!
//! The LED data pin is M-Bus pin 23 — GPIO15 on the Fire27, GPIO13 on the
//! CoreS3 (the BSP's `board.sk6812` resolves the right one). Battery is the
//! bottom's IP5306 on Fire27 and the onboard AXP2101 on CoreS3. On CoreS3 the
//! LED 5 V rail is off by default and enabled via the AW9523B (with the
//! shared-VBUS contention guard).
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use common::{STRIP_BYTES, draw_panel, wheel};
use demos::board::{self, NAME};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use m5stack_core::driver::sk6812::{Rgb, Sk6812Driver};
use static_cell::make_static;

// Panic handler + log/console transport come from the BSP (the panic-handler +
// console-serial features); the app descriptor is the one line the binary keeps.
m5stack_core::app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    demos::boot!(spawner, board, Default);

    let (mut display, i2c) = board::init_display(board.spi2, board.i2c0).await;

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);

    // CoreS3 only: bring up the M-Bus 5 V rail that powers the LED bars.
    #[cfg(feature = "cores3")]
    let bus_5v_on = board::enable_bus_5v(i2c).await;

    // SK6812 LED bars on M-Bus pin 23 (board.sk6812 = GPIO15 / GPIO13).
    // Best-effort: if the bottom isn't attached, the writes go nowhere.
    let mut leds = Sk6812Driver::new(board.rmt, board.sk6812)
        .inspect_err(|e| log::info!("SK6812 init failed: {:?}", e))
        .ok();
    let mut led_step: u8 = 0;

    loop {
        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::string::String::from(board::battery_line(i2c).await.as_str()));
        #[cfg(feature = "cores3")]
        lines.push(alloc::format!("5V bus: {}", if bus_5v_on { "ON" } else { "OFF" }));
        lines.push(alloc::string::String::from("LEDs cycle on M-Bus 23"));
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_panel(&mut display, &mut strip_buf[..], NAME, "M5GO", &refs).await;

        // Rotate a colour wheel across the 10 SK6812 LEDs on the bottom.
        if let Some(leds) = leds.as_mut() {
            let mut frame = [Rgb::OFF; 10];
            for (i, px) in frame.iter_mut().enumerate() {
                let (r, g, b) = wheel(led_step.wrapping_add((i as u8) * 25));
                // ~1/4 brightness — comfortable behind the frosted diffusers.
                *px = Rgb::new(r >> 2, g >> 2, b >> 2);
            }
            if let Err(e) = leds.write(&frame).await {
                log::info!("SK6812 write failed: {:?}", e);
            }
            led_step = led_step.wrapping_add(8);
        }

        Timer::after(Duration::from_millis(500)).await;
    }
}
