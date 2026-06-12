// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fire27 — DS18B20 1-Wire temperature read over RMT (HIL test for the
//! vendored `m5stack_core::driver::onewire` / `ds18b20`).
//!
//! Wiring: DS18B20 sensors on **Port B (black)**, data line on **G26** (the
//! signal pin following VCC; the other Port-B pin, G36, is input-only and
//! can't drive the bidirectional bus). External 4.7k pull-up to 3V3 required.
//! Expects up to two sensors on the bus; the ROM search enumerates them.
//!
//! Run: `cargo +esp run --release -p fire27 --bin onewire`
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::AnyPin,
    interrupt::software::SoftwareInterruptControl,
    ram,
    timer::timg::TimerGroup,
};
use esp_println::println;
use m5stack_core::driver::ds18b20::Ds18b20Driver;

esp_bootloader_esp_idf::esp_app_desc!();

#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    loop {}
}

/// esp-println's `timestamp` feature calls this for the log prefix; back it with
/// the embassy monotonic clock (valid once `esp_rtos::start` has run).
#[unsafe(no_mangle)]
extern "Rust" fn _esp_println_timestamp() -> u64 {
    embassy_time::Instant::now().as_millis()
}

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // Small reclaimed-ROM heap (esp-rtos needs an allocator); no PSRAM/WiFi here.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 32 * 1024);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

    // Surfaces the driver's internal debug!/warn! over serial (build with
    // ESP_LOG=debug). The "found device" lines appear only on the first cycle
    // (single ROM scan, then cached); a disconnected sensor logs a `lost` warn.
    esp_println::logger::init_logger_from_env();

    println!("== DS18B20 1-Wire test — data on G26 (Port B / black) ==");
    let mut ds = match Ds18b20Driver::new(peripherals.RMT, AnyPin::from(peripherals.GPIO26)) {
        Ok(d) => d,
        Err(e) => {
            println!("ds18b20 init failed: {:?}", e);
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
                    println!("  sensor {:#018x} = {} C", addr.0, temp);
                    n += 1;
                }
                println!("-> found {} DS18B20 sensor(s)", n);
            }
            Err(e) => println!("read error: {:?}", e),
        }
        Timer::after(Duration::from_secs(2)).await;
    }
}
