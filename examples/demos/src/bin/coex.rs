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

use common::{STRIP_BYTES, draw_demo, draw_status};
use demos::board::{self, NAME};
use demos::{ble, shim};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use m5stack_core::driver::radio::ble::BleRadio;
use m5stack_core::driver::radio::wifi::{self, WifiControl};
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

/// WiFi credentials, supplied at build time. When `WIFI_SSID` is unset WiFi is
/// skipped, but the BLE scanner still runs.
const WIFI_SSID: Option<&str> = option_env!("WIFI_SSID");
const WIFI_PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();
    shim::init_logger();
    let board = board::Board::split(p);
    shim::init_heaps_coex(board.psram);
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);

    // --- WiFi (STA + DHCP) ---
    let mut wifi_stack: Option<embassy_net::Stack<'static>> = None;
    match board::connect_wifi(board.wifi, WIFI_SSID, WIFI_PASSWORD) {
        Some((stack, control, runner)) => {
            wifi_stack = Some(stack);
            spawner.spawn(wifi::wifi_task(runner).unwrap());
            spawner.spawn(net_demo(stack, control).unwrap());
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
    draw_demo(&mut display, &mut strip_buf[..], NAME, &["WiFi + BLE"]).await;

    // --- Status loop: the DHCP IP and discovered BLE peer MACs ---
    loop {
        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::format!("{} coex", NAME));
        match wifi_stack.and_then(|s| s.config_v4()) {
            Some(cfg) => lines.push(alloc::format!("IP {}", cfg.address)),
            None => lines.push(alloc::string::String::from(if wifi_stack.is_some() {
                "WiFi: connecting..."
            } else {
                "WiFi: disabled"
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
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_status(&mut display, &mut strip_buf[..], &refs).await;
        Timer::after(Duration::from_millis(500)).await;
    }
}

/// Wait for a DHCP lease, log the IP, then scan for nearby APs.
#[embassy_executor::task]
async fn net_demo(stack: embassy_net::Stack<'static>, control: WifiControl) {
    log::info!("WiFi: connecting + waiting for DHCP...");
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        log::info!("WiFi: got IP {}", cfg.address);
    }
    match control.scan().await {
        Ok(aps) => {
            log::info!("WiFi scan: {} AP(s)", aps.len());
            for ap in &aps {
                log::info!(
                    "  {:<32} ch{:>2} {:>4} dBm",
                    ap.ssid.as_str(),
                    ap.channel,
                    ap.signal_strength
                );
            }
        }
        Err(e) => log::info!("WiFi scan failed: {:?}", e),
    }
}
