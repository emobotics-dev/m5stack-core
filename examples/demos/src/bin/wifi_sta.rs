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

use common::{STRIP_BYTES, draw_demo, draw_status};
use demos::board::{self, NAME};
use demos::shim;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
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

/// WiFi credentials, supplied at build time. When `WIFI_SSID` is unset the bin
/// skips WiFi and just runs the display.
const WIFI_SSID: Option<&str> = option_env!("WIFI_SSID");
const WIFI_PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();
    shim::init_logger();
    let board = board::Board::split(p);
    shim::init_heaps_default(board.psram);
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);

    // `Stack` is `Copy`, so keep a handle for the on-screen IP readout and spawn
    // the runner + a net-demo task.
    let mut wifi_stack: Option<embassy_net::Stack<'static>> = None;
    match board::connect_wifi(board.wifi, WIFI_SSID, WIFI_PASSWORD) {
        Some((stack, control, runner)) => {
            wifi_stack = Some(stack);
            spawner.spawn(wifi::wifi_task(runner).unwrap());
            spawner.spawn(net_demo(stack, control).unwrap());
        }
        None => log::info!("WiFi disabled (set WIFI_SSID/WIFI_PASSWORD to enable)"),
    }

    let (mut display, _i2c) = board::init_display(board.spi2, board.i2c0).await;

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);
    draw_demo(&mut display, &mut strip_buf[..], NAME, &["WiFi STA"]).await;

    loop {
        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::format!("{} WiFi STA", NAME));
        match wifi_stack.and_then(|s| s.config_v4()) {
            Some(cfg) => lines.push(alloc::format!("IP {}", cfg.address)),
            None => lines.push(alloc::string::String::from(if wifi_stack.is_some() {
                "WiFi: connecting..."
            } else {
                "WiFi: disabled"
            })),
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
