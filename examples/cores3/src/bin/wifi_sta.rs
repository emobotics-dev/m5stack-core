// SPDX-License-Identifier: MIT OR Apache-2.0
//! CoreS3 — WiFi STA + DHCP + scan.
//!
//! Set `WIFI_SSID`/`WIFI_PASSWORD` at build time to join a network (unset →
//! WiFi skipped, display still runs):
//! `WIFI_SSID=ssid WIFI_PASSWORD=pw cargo +esp run --release -p cores3 --bin wifi_sta --target xtensa-esp32s3-none-elf`
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use cores3::{Lcd, STRIP_BYTES, connect_wifi, draw_demo, draw_status, init_display, init_i2c};
use embassy_time::{Duration, Timer};
use esp_hal::{
    gpio::AnyPin, interrupt::software::SoftwareInterruptControl, ram, timer::timg::TimerGroup,
};
use m5stack_core::driver::radio::wifi::{self, WifiControl};
use panic_halt as _;
use rtt_target::rprintln;
use static_cell::make_static;

esp_bootloader_esp_idf::esp_app_desc!();

/// WiFi credentials, supplied at build time. When `WIFI_SSID` is unset the bin
/// skips WiFi and just runs the display.
const WIFI_SSID: Option<&str> = option_env!("WIFI_SSID");
const WIFI_PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) {
    // CRITICAL: esp_hal::init() MUST come before rtt_init_print!()
    let peripherals = esp_hal::init(esp_hal::Config::default());
    rtt_target::rtt_init_print!();
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
    // WiFi keeps its RX/TX buffers in internal SRAM. NOTE: esp-alloc's global
    // heap holds at most 3 regions — this internal heap, the reclaimed region
    // above, and the PSRAM region (below) are exactly 3, so do NOT add a 4th
    // `heap_allocator!`.
    esp_alloc::heap_allocator!(size: 64 * 1024);
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
        None => rprintln!("WiFi disabled (set WIFI_SSID/WIFI_PASSWORD to enable)"),
    }

    let i2c_bus = init_i2c(
        peripherals.I2C0,
        AnyPin::from(peripherals.GPIO12),
        AnyPin::from(peripherals.GPIO11),
    );

    let shared_spi = make_static!(None);
    let (mut display, _axp): (Lcd, _) = init_display(
        i2c_bus,
        shared_spi,
        peripherals.SPI2,
        AnyPin::from(peripherals.GPIO36),
        AnyPin::from(peripherals.GPIO37),
        AnyPin::from(peripherals.GPIO3),
        AnyPin::from(peripherals.GPIO35),
    )
    .await;

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);
    draw_demo(&mut display, &mut strip_buf[..], "CoreS3", &["WiFi STA"]).await;

    loop {
        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::string::String::from("CoreS3 WiFi STA"));
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

/// Wait for a DHCP lease, print the IP, then scan for nearby APs (rprintln).
#[embassy_executor::task]
async fn net_demo(stack: embassy_net::Stack<'static>, control: WifiControl) {
    rprintln!("WiFi: connecting + waiting for DHCP...");
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        rprintln!("WiFi: got IP {}", cfg.address);
    }
    match control.scan().await {
        Ok(aps) => {
            rprintln!("WiFi scan: {} AP(s)", aps.len());
            for ap in &aps {
                rprintln!(
                    "  {:<32} ch{:>2} {:>4} dBm",
                    ap.ssid.as_str(),
                    ap.channel,
                    ap.signal_strength
                );
            }
        }
        Err(e) => rprintln!("WiFi scan failed: {:?}", e),
    }
}
