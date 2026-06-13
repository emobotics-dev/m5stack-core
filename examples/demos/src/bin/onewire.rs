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

use demos::board;
use demos::shim;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use m5stack_core::driver::ds18b20::Ds18b20Driver;

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

/// esp-println's `timestamp` feature calls this for the log prefix; back it with
/// the embassy monotonic clock (valid once `esp_rtos::start` has run).
#[unsafe(no_mangle)]
extern "Rust" fn _esp_println_timestamp() -> u64 {
    embassy_time::Instant::now().as_millis()
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) {
    let p = board::init();
    shim::init_logger();
    let board = board::Board::split(p);
    shim::init_heap_onewire();
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);

    log::info!("== DS18B20 1-Wire test — data on G26 (Port B / black) ==");
    let mut ds = match Ds18b20Driver::new(board.rmt, board.m5bus.gpio26) {
        Ok(d) => d,
        Err(e) => {
            log::error!("ds18b20 init failed: {:?}", e);
            loop {
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    };

    loop {
        match ds.read_all_temperatures().await {
            Ok(temps) => {
                let mut n = 0u32;
                for (addr, temp) in temps {
                    log::info!("  sensor {:#018x} = {} C", addr.0, temp);
                    n += 1;
                }
                log::info!("-> found {} DS18B20 sensor(s)", n);
            }
            Err(e) => log::error!("read error: {:?}", e),
        }
        Timer::after(Duration::from_secs(2)).await;
    }
}
