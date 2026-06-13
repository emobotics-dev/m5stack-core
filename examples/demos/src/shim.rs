// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-board logger/panic + heap setup, so the bins read identically.

use esp_hal::peripherals::PSRAM;
use esp_hal::ram;

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

// esp-alloc's global heap holds at most 3 regions; each profile registers the
// reclaimed-ROM region, the plain-DRAM region, and the external PSRAM region
// (exactly 3) — do NOT add a 4th. Sizes are the HIL-proven per-bin values.

/// Display / I2C / WiFi bins: 50 KiB reclaimed + 64 KiB plain + PSRAM.
pub fn init_heaps_default(psram: PSRAM<'static>) {
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
    esp_alloc::heap_allocator!(size: 64 * 1024);
    m5stack_core::mem::init_psram_heap(psram);
}

/// Coex (WiFi + BLE) needs more controller heap. Fire27 → 96 KiB reclaimed +
/// 24 KiB plain; CoreS3 → 50 KiB reclaimed + 96 KiB plain. Plus PSRAM.
pub fn init_heaps_coex(psram: PSRAM<'static>) {
    #[cfg(feature = "fire27")]
    {
        esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 96 * 1024);
        esp_alloc::heap_allocator!(size: 24 * 1024);
    }
    #[cfg(feature = "cores3")]
    {
        esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
        esp_alloc::heap_allocator!(size: 96 * 1024);
    }
    m5stack_core::mem::init_psram_heap(psram);
}

/// LVGL bin: 50 KiB reclaimed (LVGL's object/style pool); no PSRAM.
pub fn init_heap_lvgl() {
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
}
