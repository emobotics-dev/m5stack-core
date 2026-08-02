// SPDX-License-Identifier: MIT OR Apache-2.0
//! Backend for LVGL's own profiler hooks (`conf/m5_profiler.h`).
//!
//! Records **exclusive** time per tag — inclusive time alone just re-counts the
//! nesting and cannot say which frame actually spends the cycles.
//!
//! Called only from the render thread: LVGL is single-threaded here, which is
//! what lets the stack be a plain static. The totals are atomics because the
//! reporting task reads them from another thread (and, in the APP-core build,
//! another core).

use core::ffi::{CStr, c_char};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

/// Distinct tags tracked. LVGL's refresh path uses well under this; anything
/// beyond it is dropped rather than silently mis-attributed.
const MAX_TAGS: usize = 96;
/// Nesting depth. LVGL's refresh nests a handful deep.
const MAX_DEPTH: usize = 64;

static KEYS: [AtomicUsize; MAX_TAGS] = [const { AtomicUsize::new(0) }; MAX_TAGS];
static EXCL_US: [AtomicU32; MAX_TAGS] = [const { AtomicU32::new(0) }; MAX_TAGS];
static CALLS: [AtomicU32; MAX_TAGS] = [const { AtomicU32::new(0) }; MAX_TAGS];
/// Tags seen but not recorded, because the table or the stack was full. Nonzero
/// means the numbers are incomplete and must not be read as a full account.
pub static DROPPED: AtomicU32 = AtomicU32::new(0);

/// One open BEGIN: which tag, when it started, and how much of that has since
/// been attributed to nested tags.
#[derive(Clone, Copy)]
struct Frame {
    slot: usize,
    start_us: u64,
    child_us: u64,
}

static mut STACK: [Frame; MAX_DEPTH] = [Frame { slot: 0, start_us: 0, child_us: 0 }; MAX_DEPTH];
static mut DEPTH: usize = 0;

/// Find or claim the slot for a tag pointer.
fn slot_for(key: usize) -> Option<usize> {
    for i in 0..MAX_TAGS {
        let k = KEYS[i].load(Relaxed);
        if k == key {
            return Some(i);
        }
        if k == 0 && KEYS[i].compare_exchange(0, key, Relaxed, Relaxed).is_ok() {
            return Some(i);
        }
    }
    None
}

#[unsafe(no_mangle)]
pub extern "C" fn m5_prof_begin(tag: *const c_char) {
    let Some(slot) = slot_for(tag as usize) else {
        DROPPED.fetch_add(1, Relaxed);
        return;
    };
    // SAFETY: render thread only — LVGL makes every profiler call from the one
    // thread that owns it.
    unsafe {
        let depth = &mut *core::ptr::addr_of_mut!(DEPTH);
        if *depth >= MAX_DEPTH {
            DROPPED.fetch_add(1, Relaxed);
            return;
        }
        let stack = &mut *core::ptr::addr_of_mut!(STACK);
        stack[*depth] = Frame { slot, start_us: esp_radio_rtos_driver::now(), child_us: 0 };
        *depth += 1;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn m5_prof_end(_tag: *const c_char) {
    // SAFETY: as above — single-threaded with respect to LVGL.
    unsafe {
        let depth = &mut *core::ptr::addr_of_mut!(DEPTH);
        if *depth == 0 {
            return;
        }
        *depth -= 1;
        let stack = &mut *core::ptr::addr_of_mut!(STACK);
        let frame = stack[*depth];
        let elapsed = esp_radio_rtos_driver::now() - frame.start_us;
        let exclusive = elapsed.saturating_sub(frame.child_us);
        EXCL_US[frame.slot].fetch_add(exclusive as u32, Relaxed);
        CALLS[frame.slot].fetch_add(1, Relaxed);
        // Charge the whole span to the parent so its own exclusive time excludes
        // this one.
        if *depth > 0 {
            stack[*depth - 1].child_us += elapsed;
        }
    }
}

/// Log the tags that spent the most exclusive time in the last interval, then
/// reset. `total_us` is the window, used to express each as a percentage.
pub fn report(total_us: u32, top: usize) {
    let mut rows: [(usize, u32, u32); MAX_TAGS] = [(0, 0, 0); MAX_TAGS];
    let mut n = 0;
    for i in 0..MAX_TAGS {
        let key = KEYS[i].load(Relaxed);
        let us = EXCL_US[i].swap(0, Relaxed);
        let calls = CALLS[i].swap(0, Relaxed);
        if key != 0 && us > 0 {
            rows[n] = (key, us, calls);
            n += 1;
        }
    }
    rows[..n].sort_unstable_by(|a, b| b.1.cmp(&a.1));

    let dropped = DROPPED.swap(0, Relaxed);
    if dropped > 0 {
        log::warn!("[lvprof] {} tags dropped — totals are incomplete", dropped);
    }
    for &(key, us, calls) in rows.iter().take(top.min(n)) {
        // SAFETY: `key` is a tag pointer LVGL passed us, a `__func__` or literal
        // with static storage duration.
        let name = unsafe { CStr::from_ptr(key as *const c_char) }.to_str().unwrap_or("?");
        log::info!(
            "[lvprof] {:<28} {:>3}% excl={}us calls={} us/call={}",
            name,
            us * 100 / total_us.max(1),
            us,
            calls,
            us / calls.max(1),
        );
    }
}
