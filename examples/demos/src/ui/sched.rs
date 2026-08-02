// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where the UI runs, and how it ranks against everything else (#63).
//!
//! Threads, not another `InterruptExecutor`: an interrupt executor makes the UI
//! preempt *everything*, which is backwards — latency-sensitive work has to win.
//! A thread is preemptible by priority in both directions.

use core::ffi::c_void;

/// Task priorities. `#[esp_rtos::main]` starts at 0 — the **lowest** — so the app
/// executor must be raised explicitly or the UI threads would outrank the very
/// work they exist to yield to.
///
/// The whole ladder stays inside 1..=3 on purpose: esp-radio's blob threads are
/// created at the priority the blob asks for (ESP-IDF convention puts them in the
/// low 20s), so keeping the UI down here means it cannot starve the radio.
pub const PRIO_APP: usize = 3;
pub const PRIO_FLUSH: u32 = 2;
pub const PRIO_RENDER: u32 = 1;

/// LVGL's draw path is the deep one; the flush side only moves bytes. Sized from
/// oxicharge#93's measurements (20 KiB render / 8 KiB flush, ~27.7 kB and 8.6 kB
/// of SRAM actually consumed), not guessed.
pub const RENDER_STACK: usize = 20 * 1024;
pub const FLUSH_STACK: usize = 8 * 1024;

/// Raise the calling thread — the `#[esp_rtos::main]` executor — above the UI.
pub fn raise_app_executor() {
    esp_rtos::CurrentThreadHandle::get().set_priority(PRIO_APP);
}

/// Which core the render thread runs on. LVGL stays single-threaded either way —
/// one thread makes every LVGL call; `ui-app-core` only changes which core that
/// thread sits on, moving the rasterisation cost off PRO.
#[cfg(feature = "ui-app-core")]
pub const RENDER_CORE: u32 = 1;
#[cfg(all(not(feature = "ui-app-core")))]
pub const RENDER_CORE: u32 = 0;

/// Spawn a native thread pinned to `core`.
///
/// # Safety
/// `entry` must never return: the thread is never joined and never deleted.
pub unsafe fn spawn(
    name: &str,
    entry: extern "C" fn(*mut c_void),
    prio: u32,
    stack: usize,
    core: u32,
) {
    unsafe {
        esp_radio_rtos_driver::task_create(
            name,
            entry,
            core::ptr::null_mut(),
            prio,
            Some(core),
            stack,
        );
    }
}
