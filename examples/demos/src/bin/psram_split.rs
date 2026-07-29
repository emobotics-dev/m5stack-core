// SPDX-License-Identifier: MIT OR Apache-2.0
//! `mem::psram_split` — carve a **private** PSRAM region for a foreign allocator
//! (e.g. LVGL's TLSF) while the remainder backs the global heap.
//!
//! Log-only (no display). It demonstrates the API and self-validates on hardware:
//! 1. the private region is real, writable, contiguous PSRAM (per-page R/W sweep);
//! 2. it is **disjoint** from the global-heap remainder (a large external
//!    `psram_vec` is filled, then the private pattern is re-read intact);
//! 3. the remainder is registered (the external `psram_vec` allocates from it).
//!
//! Build: `cargo +esp run --release -p demos --bin psram_split` (Fire27) or
//! `--no-default-features --features cores3 --target xtensa-esp32s3-none-elf`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use demos::board;
use demos::shim;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use m5stack_core::mem::{self, HeapProfile};

m5stack_core::app_desc!();

/// Private pool to carve — 512 KiB, the shape oxivgl#116 sizes LVGL's TLSF at.
const RESERVE: usize = 512 * 1024;
/// R/W sweep stride — one touch per 4 KiB page across the whole private span.
const STRIDE: usize = 4096;
/// External allocation used to prove disjointness from the private region.
const EXT_PROBE: usize = 256 * 1024;

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();
    let board = board::Board::split(p);
    // DRAM heap only — `init_heap` never touches PSRAM. PSRAM is handed to
    // `psram_split` below instead.
    mem::init_heap(HeapProfile::Default);
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);
    #[cfg(feature = "fire27")]
    let _console = shim::init_console(
        spawner,
        board::console_serial(board.uart0, board.uart0_rx, board.uart0_tx),
    );
    #[cfg(feature = "cores3")]
    let _console = shim::init_console(spawner, board::console_serial(board.usb_device));

    log::info!("[psram] split: reserving {} KiB private", RESERVE / 1024);
    let ok = match mem::psram_split(board.psram, RESERVE) {
        Ok(split) => validate(split),
        Err(e) => {
            log::error!("[psram] split FAILED: {:?}", e);
            false
        }
    };
    log::info!("[psram] RESULT: {}", if ok { "PASS" } else { "FAIL" });

    loop {
        log::info!("[psram] {} (idle)", if ok { "PASS" } else { "FAIL" });
        Timer::after(Duration::from_secs(5)).await;
    }
}

/// Per-page test pattern, distinct per 4 KiB page so an aliased/wrapped mapping
/// shows up as a mismatch.
fn pattern(i: usize) -> u8 {
    ((i >> 12) as u8).wrapping_mul(31) ^ 0xA5
}

fn validate(split: mem::PsramSplit) -> bool {
    let private = split.private;
    let len = private.len();
    log::info!(
        "[psram] private {} KiB @ {:#x}, global_free {} KiB",
        len / 1024,
        private.as_ptr() as usize,
        split.global_free / 1024,
    );
    if len != RESERVE {
        log::error!("[psram] private len {} != reserve {}", len, RESERVE);
        return false;
    }
    if private.as_ptr() as usize % 8 != 0 {
        log::error!("[psram] private base not 8-aligned");
        return false;
    }

    // 1. Write a per-page pattern across the whole private span.
    for i in (0..len).step_by(STRIDE) {
        private[i].write(pattern(i));
    }

    // 2. Allocate and fill a large EXTERNAL vec from the global remainder — this
    //    would clobber the private region if the two were not disjoint.
    let mut ext = mem::psram_vec::<u8>(EXT_PROBE);
    ext.resize(EXT_PROBE, 0x5A);
    core::hint::black_box(&ext);

    // 3. Re-read the private pattern: intact ⇒ private ∩ global remainder = ∅.
    let mut disjoint = true;
    for i in (0..len).step_by(STRIDE) {
        let got = unsafe { private[i].assume_init() };
        if got != pattern(i) {
            log::error!("[psram] private mismatch @ {}: {:#x} != {:#x}", i, got, pattern(i));
            disjoint = false;
            break;
        }
    }
    log::info!(
        "[psram] external psram_vec {} KiB filled; private re-read {}",
        ext.capacity() / 1024,
        if disjoint { "intact" } else { "CORRUPT" },
    );
    disjoint
}
