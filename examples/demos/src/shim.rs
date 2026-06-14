// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-board logger + heap setup, so the bins read identically. Logging now goes
//! through the BSP console (`io::console::install`) on both boards — Fire27 over
//! UART0, CoreS3 over the probe-free USB-Serial-JTAG CDC (#31). The BSP also
//! provides the `#[panic_handler]` (the `panic-handler` feature) so the bins
//! carry no panic boilerplate.

use embassy_executor::Spawner;
use esp_hal::peripherals::PSRAM;
use m5stack_core::io::console::{self, Config, Console, SerialResources};
use m5stack_core::mem::{self, HeapProfile};

/// Bring the BSP console up (`log` backend + transport + drain) and report any
/// previous-run panic breadcrumb. Replaces the old esp-println / RTT glue.
/// Call once from main with the chip's serial bundle (see `board::console_serial`).
pub fn init_console(spawner: Spawner, serial: SerialResources) -> Console {
    // R8: surface a prior panic ONCE, after the backend is up so it reaches the
    // console (and the host's `detect_crash` grep).
    let crumb = console::take_panic_breadcrumb();
    let console =
        console::install(spawner, Config { serial: Some(serial), level: log::LevelFilter::Info });
    if let Some(c) = crumb {
        log::warn!("{} @ {} (reason {:#010x})", console::markers::PREV_PANIC, c.location, c.reason);
    }
    console
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
