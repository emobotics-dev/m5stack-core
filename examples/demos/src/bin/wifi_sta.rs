// SPDX-License-Identifier: MIT OR Apache-2.0
//! WiFi STA + DHCP + scan.
//!
//! Set `WIFI_SSID`/`WIFI_PASSWORD` at build time to join a network (unset →
//! WiFi skipped, display still runs), e.g.:
//! `WIFI_SSID=ssid WIFI_PASSWORD=pw cargo +esp run --release -p demos --bin wifi_sta`
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use common::{STRIP_BYTES, draw_panel};
use demos::board::{self, NAME};
use demos::{net, shim};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use m5stack_core::driver::radio::wifi;
use static_cell::make_static;

// Panic handler + log/console transport come from the BSP (the panic-handler +
// console-serial features); the app descriptor is the one line the binary keeps.
m5stack_core::app_desc!();

/// WiFi credentials, supplied at build time. When `WIFI_SSID` is unset the bin
/// skips WiFi and just runs the display.
const WIFI_SSID: Option<&str> = option_env!("WIFI_SSID");
const WIFI_PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();
    let board = board::Board::split(p);
    shim::init_heaps_default();
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);
    #[cfg(feature = "fire27")]
    let _console =
        shim::init_console(spawner, board::console_serial(board.uart0, board.uart0_rx, board.uart0_tx));
    #[cfg(feature = "cores3")]
    let _console = shim::init_console(spawner, board::console_serial(board.usb_device));

    // `Stack` is `Copy`, so keep a handle for the on-screen IP readout and spawn
    // the runner + a net-demo task.
    let mut wifi_stack: Option<embassy_net::Stack<'static>> = None;
    match board::connect_wifi(board.wifi, WIFI_SSID, WIFI_PASSWORD) {
        Some((stack, control, runner)) => {
            wifi_stack = Some(stack);
            spawner.spawn(wifi::wifi_task(runner).unwrap());
            spawner.spawn(net::net_demo(stack, control).unwrap());
        }
        None => log::info!("WiFi disabled (set WIFI_SSID/WIFI_PASSWORD to enable)"),
    }

    let (mut display, _i2c) = board::init_display(board.spi2, board.i2c0).await;

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);

    loop {
        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        match wifi_stack.and_then(|s| s.config_v4()) {
            Some(cfg) => lines.push(alloc::format!("IP {}", cfg.address)),
            None => lines.push(alloc::string::String::from(if wifi_stack.is_some() {
                "connecting..."
            } else {
                "WiFi disabled"
            })),
        }
        // Nearby networks discovered by the one-shot scan (net::net_demo).
        let aps = net::scan_lines();
        if !aps.is_empty() {
            lines.push(alloc::string::String::from(""));
            lines.push(alloc::string::String::from("Nearby APs:"));
            for ap in &aps {
                lines.push(alloc::string::String::from(ap.as_str()));
            }
        }
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_panel(&mut display, &mut strip_buf[..], NAME, "WiFi STA", &refs).await;
        Timer::after(Duration::from_millis(500)).await;
    }
}
