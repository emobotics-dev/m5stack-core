// SPDX-License-Identifier: MIT OR Apache-2.0
//! WiFi + BLE coexistence: a BLE peer-MAC scanner alongside the WiFi station,
//! listing discovered MACs on the LCD under the IP.
//!
//! Gated by `required-features = ["coex"]` and **must be built on its own**:
//! `cargo +esp run --release -p demos --bin coex --features coex` (Fire27) or add
//! `--no-default-features --features cores3,coex --target xtensa-esp32s3-none-elf`.
//! esp-radio's coexist blob is a crate-global link dependency that only this
//! BLE-initialising bin can satisfy, so enabling `--features coex` while building
//! the *other* (non-BLE) bins fails to link — build this one with `--bin coex`.
//! (Plain `cargo build` / `--workspace` leave `coex` off and are unaffected.)
//! Build `--release` (the BLE deps trip a dev-profile xtensa codegen bug).
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use common::{STRIP_BYTES, draw_panel};
use demos::board::{self, NAME};
use demos::{ble, net, shim};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use m5stack_core::driver::radio::ble::BleRadio;
use m5stack_core::driver::radio::wifi;
use static_cell::make_static;

// Panic handler + log/console transport come from the BSP (the panic-handler +
// console-serial features); the app descriptor is the one line the binary keeps.
m5stack_core::app_desc!();

/// WiFi credentials, supplied at build time. When `WIFI_SSID` is unset WiFi is
/// skipped, but the BLE scanner still runs.
const WIFI_SSID: Option<&str> = option_env!("WIFI_SSID");
const WIFI_PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();
    let board = board::Board::split(p);
    shim::init_heaps_coex();
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);
    #[cfg(feature = "fire27")]
    let _console =
        shim::init_console(spawner, board::console_serial(board.uart0, board.uart0_rx, board.uart0_tx));
    #[cfg(feature = "cores3")]
    let _console = shim::init_console(spawner, board::console_serial(board.usb_device));

    // --- WiFi (STA + DHCP) ---
    let mut wifi_stack: Option<embassy_net::Stack<'static>> = None;
    match board::connect_wifi(board.wifi, WIFI_SSID, WIFI_PASSWORD) {
        Some((stack, control, runner)) => {
            wifi_stack = Some(stack);
            spawner.spawn(wifi::wifi_task(runner).unwrap());
            spawner.spawn(net::net_demo(stack, control).unwrap());
        }
        None => log::info!("WiFi disabled (set WIFI_SSID/WIFI_PASSWORD to enable)"),
    }

    // --- BLE peer-MAC scanner (coexistence) ---
    match BleRadio::new(board.bt) {
        Ok(radio) => {
            spawner.spawn(ble::ble_scan_task(radio).unwrap());
        }
        Err(e) => log::info!("BLE init failed: {:?}", e),
    }

    let (mut display, _i2c) = board::init_display(board.spi2, board.i2c0).await;

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);

    // --- Status loop: DHCP IP, discovered BLE peer MACs, and nearby APs ---
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
        lines.push(alloc::string::String::from("BLE peers:"));
        for mac in ble::snapshot() {
            // Conventional MSB-first notation (raw() is little-endian).
            lines.push(alloc::format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[5], mac[4], mac[3], mac[2], mac[1], mac[0]
            ));
        }
        let aps = net::scan_lines();
        if !aps.is_empty() {
            lines.push(alloc::string::String::from("APs:"));
            for ap in &aps {
                lines.push(alloc::string::String::from(ap.as_str()));
            }
        }
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_panel(&mut display, &mut strip_buf[..], NAME, "WiFi+BLE", &refs).await;
        Timer::after(Duration::from_millis(500)).await;
    }
}
