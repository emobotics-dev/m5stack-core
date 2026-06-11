// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fire27 — WiFi + BLE coexistence.
//!
//! Runs the WiFi STA bring-up plus a BLE peer-MAC scanner, listing discovered
//! MACs on the LCD under the IP. Build `--release` (the BLE deps trip a
//! dev-profile xtensa codegen bug):
//! `cargo +esp run --release -p fire27 --bin coex --features coex`
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock, gpio::AnyPin, interrupt::software::SoftwareInterruptControl, ram,
    timer::timg::TimerGroup,
};
use esp_println as _;
use fire27::{Lcd, STRIP_BYTES, ble, connect_wifi, draw_demo, draw_status, init_display};
use log::info;
use m5stack_core::driver::radio::ble::BleRadio;
use m5stack_core::driver::radio::wifi::{self, WifiControl};
use static_cell::make_static;

esp_bootloader_esp_idf::esp_app_desc!();

/// WiFi credentials, supplied at build time. When `WIFI_SSID` is unset WiFi is
/// skipped, but the BLE scanner still runs.
const WIFI_SSID: Option<&str> = option_env!("WIFI_SSID");
const WIFI_PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    info!("custom_halt — resetting");
    loop {}
}

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // Coex (WiFi + BLE) needs more controller heap, so the reclaimed region is
    // larger and the plain-DRAM heap smaller. NOTE: esp-alloc's global heap holds
    // at most 3 regions — these two plus the PSRAM region (below) are exactly 3,
    // so do NOT add a 4th `heap_allocator!`.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 96 * 1024);
    esp_alloc::heap_allocator!(size: 24 * 1024);
    // The ESP32 cannot DMA out of PSRAM, so WiFi buffers and the framebuffer
    // stay in internal/reclaimed SRAM; PSRAM is registered only for the heap.
    m5stack_core::mem::init_psram_heap(peripherals.PSRAM);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

    // --- WiFi (STA + DHCP) ---
    let mut wifi_stack: Option<embassy_net::Stack<'static>> = None;
    match connect_wifi(peripherals.WIFI, WIFI_SSID, WIFI_PASSWORD) {
        Some((stack, control, runner)) => {
            wifi_stack = Some(stack);
            spawner.spawn(wifi::wifi_task(runner).unwrap());
            spawner.spawn(net_demo(stack, control).unwrap());
        }
        None => info!("WiFi disabled (set WIFI_SSID/WIFI_PASSWORD to enable)"),
    }

    // --- BLE peer-MAC scanner (coexistence) ---
    match BleRadio::new(peripherals.BT) {
        Ok(radio) => {
            spawner.spawn(ble::ble_scan_task(radio).unwrap());
        }
        Err(e) => info!("BLE init failed: {:?}", e),
    }

    let shared_spi = make_static!(None);
    let (mut display, mut bl): (Lcd, _) = init_display(
        shared_spi,
        peripherals.SPI2,
        AnyPin::from(peripherals.GPIO18),
        AnyPin::from(peripherals.GPIO23),
        AnyPin::from(peripherals.GPIO19),
        AnyPin::from(peripherals.GPIO14),
        AnyPin::from(peripherals.GPIO27),
        AnyPin::from(peripherals.GPIO33),
        AnyPin::from(peripherals.GPIO32),
    )
    .await;
    bl.set_high();

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);
    draw_demo(&mut display, &mut strip_buf[..], "Fire27", &["WiFi + BLE"]).await;

    // --- Status loop: show the DHCP IP and discovered BLE peer MACs ---
    loop {
        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::string::String::from("Fire27 coex"));
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
            lines.push(alloc::format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[5],
                mac[4],
                mac[3],
                mac[2],
                mac[1],
                mac[0]
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
    info!("WiFi: connecting + waiting for DHCP...");
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        info!("WiFi: got IP {}", cfg.address);
    }
    match control.scan().await {
        Ok(aps) => {
            info!("WiFi scan: {} AP(s)", aps.len());
            for ap in &aps {
                info!(
                    "  {:<32} ch{:>2} {:>4} dBm",
                    ap.ssid.as_str(),
                    ap.channel,
                    ap.signal_strength
                );
            }
        }
        Err(e) => info!("WiFi scan failed: {:?}", e),
    }
}
