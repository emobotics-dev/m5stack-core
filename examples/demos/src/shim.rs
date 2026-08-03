// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-board logger + heap setup, so the bins read identically. Logging now goes
//! through the BSP console (`io::console::install`) on both boards — Fire27 over
//! UART0, CoreS3 over the probe-free USB-Serial-JTAG CDC (#31). The BSP also
//! provides the `#[panic_handler]` (the `panic-handler` feature) so the bins
//! carry no panic boilerplate.

use embassy_executor::Spawner;
use m5stack_core::io::console::{self, Config, Console, SerialResources};
use m5stack_core::mem;

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

pub use m5stack_core::mem::HeapProfile;
/// Take the peripherals, split them into the board's pin map, and install the
/// heap. The first half of [`boot!`](crate::boot) — the half that needs no
/// macro, kept a function so the ordering is stated once.
///
/// Heap setup itself lives in the BSP: `mem::init_heap(profile)` owns the
/// esp-alloc DRAM regions and the per-board sizes (#35 C1). None of these demos
/// put PSRAM in front of the global allocator — it never touches the plain
/// `Vec`/`String` allocations they actually do (see #41); a bin that wants PSRAM
/// calls `mem::psram_map` / `mem::psram_split` itself.
pub fn boot_board(profile: HeapProfile) -> crate::board::Board {
    let p = crate::board::init();
    let board = crate::board::Board::split(p);
    mem::init_heap(profile);
    board
}
