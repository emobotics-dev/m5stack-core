// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-board logger/panic + heap setup, so the bins read identically.

use esp_hal::peripherals::PSRAM;
use m5stack_core::mem::{self, HeapProfile};

/// Register the `log` backend. Fire27 → esp-println (UART); CoreS3 → RTT at
/// **Info** (a per-frame DEBUG flood with no debugger draining the RTT buffer
/// back-pressures and stalls the app — HIL-confirmed). MUST be called AFTER
/// [`crate::board::init`] (CoreS3's `rtt_init_log!` needs the RTT control block,
/// set up by `esp_hal::init`).
pub fn init_logger() {
    #[cfg(feature = "fire27")]
    esp_println::logger::init_logger_from_env();
    #[cfg(feature = "cores3")]
    rtt_target::rtt_init_log!(log::LevelFilter::Info);
}

/// esp-println's `timestamp` feature calls this for the log prefix; back it with
/// the embassy monotonic clock (valid once `esp_rtos::start` has run). Defined
/// once here (Fire27 only) so every bin built with `ESP_LOG=…` links — without
/// it the `timestamp` feature leaves the symbol undefined and the link fails.
#[cfg(feature = "fire27")]
#[unsafe(no_mangle)]
extern "Rust" fn _esp_println_timestamp() -> u64 {
    embassy_time::Instant::now().as_millis()
}

// Heap setup now lives in the BSP: `mem::init_heap(profile, psram)` owns the
// esp-alloc regions + per-board sizes (#35 C1), so these are thin profile picks.

/// Display / I2C / WiFi bins: the Default profile (reclaimed + plain DRAM) + PSRAM.
pub fn init_heaps_default(psram: PSRAM<'static>) {
    mem::init_heap(HeapProfile::Default, Some(psram));
}

/// Coex (WiFi + BLE) needs more controller heap: the Coex profile + PSRAM.
pub fn init_heaps_coex(psram: PSRAM<'static>) {
    mem::init_heap(HeapProfile::Coex, Some(psram));
}

/// LVGL bin: the Lvgl profile (reclaimed-ROM only, no PSRAM).
pub fn init_heap_lvgl() {
    mem::init_heap(HeapProfile::Lvgl, None);
}
