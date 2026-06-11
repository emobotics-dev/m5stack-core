// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fire27 — WiFi STA + DHCP + scan.
//!
//! Set `WIFI_SSID`/`WIFI_PASSWORD` at build time to join a network (unset →
//! WiFi skipped, display still runs):
//! `WIFI_SSID=ssid WIFI_PASSWORD=pw cargo +esp run --release -p fire27 --bin wifi_sta`
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
use fire27::{Lcd, STRIP_BYTES, connect_wifi, draw_demo, draw_status, init_display};
use log::info;
use m5stack_core::driver::radio::wifi::{self, WifiControl};
use static_cell::make_static;

esp_bootloader_esp_idf::esp_app_desc!();

/// WiFi credentials, supplied at build time. When `WIFI_SSID` is unset the bin
/// skips WiFi and just runs the display.
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
    // ESP32 DRAM is tight; bulk WiFi heap goes in reclaimed ROM RAM. NOTE:
    // esp-alloc's global heap holds at most 3 regions — these two plus the PSRAM
    // region (below) are exactly 3, so do NOT add a 4th `heap_allocator!`.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
    esp_alloc::heap_allocator!(size: 64 * 1024);
    // The ESP32 cannot DMA out of PSRAM, so WiFi buffers and the framebuffer
    // stay in internal/reclaimed SRAM; PSRAM is registered only for the heap.
    m5stack_core::mem::init_psram_heap(peripherals.PSRAM);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

    // --- WiFi (STA + DHCP) --- `Stack` is `Copy`, so we keep a handle for the
    // on-screen IP readout and spawn the runner + a net-demo task.
    let mut wifi_stack: Option<embassy_net::Stack<'static>> = None;
    match connect_wifi(peripherals.WIFI, WIFI_SSID, WIFI_PASSWORD) {
        Some((stack, control, runner)) => {
            wifi_stack = Some(stack);
            spawner.spawn(wifi::wifi_task(runner).unwrap());
            spawner.spawn(net_demo(stack, control).unwrap());
        }
        None => info!("WiFi disabled (set WIFI_SSID/WIFI_PASSWORD to enable)"),
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
    draw_demo(&mut display, &mut strip_buf[..], "Fire27", &["WiFi STA"]).await;

    loop {
        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::string::String::from("Fire27 WiFi STA"));
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
