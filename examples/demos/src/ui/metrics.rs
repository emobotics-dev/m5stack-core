// SPDX-License-Identifier: MIT OR Apache-2.0
//! Counters for the A/B measurement: what the UI achieved, and what it cost
//! everyone else.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use oxivgl::flush_pipeline::{FlushSync, SemaphoreFlushSync};

/// Completed LVGL refreshes, counted by [`count_frames_on_refresh`].
pub static FRAMES: AtomicU32 = AtomicU32::new(0);
/// Pixel bytes and transfers handed to the panel. These wrap after ~15 min at
/// full rate; every reader takes a wrapping delta, so wrapping is harmless.
pub static FLUSH_BYTES: AtomicU32 = AtomicU32::new(0);
pub static FLUSH_OPS: AtomicU32 = AtomicU32::new(0);

/// How late the latency probe ran against its own deadline. This — not the frame
/// rate — is the number #63 is about.
pub static LATENCY: Latency = Latency::new();

/// Microseconds inside `lv_timer_handler`, and of those, microseconds blocked
/// waiting for a flush. The difference is LVGL's actual draw cost.
pub static HANDLER_US: AtomicU32 = AtomicU32::new(0);
/// How many times `lv_timer_handler` was called. If cost tracks calls rather
/// than pixels, the render loop's polling rate is the lever, not the drawing.
pub static HANDLER_CALLS: AtomicU32 = AtomicU32::new(0);
pub static WAIT_US: AtomicU32 = AtomicU32::new(0);

/// Count one [`FRAMES`] per completed refresh, from LVGL's own event.
///
/// Ask LVGL rather than infer from the flush counters. A refresh becomes as many
/// transfers as the dirty areas need, and with double buffering they complete
/// *after* the `lv_timer_handler` call that submitted them — so counting
/// transfers reports 0 fps for a one-area load and several times the refresh
/// rate for an eight-object one. `LV_EVENT_REFR_READY` is one refresh, fired on
/// the render thread, however LVGL split it.
///
/// # Safety
/// Call once, on the render thread, after the display exists.
pub unsafe fn count_frames_on_refresh() {
    unsafe extern "C" fn on_refr_ready(_e: *mut oxivgl_sys::lv_event_t) {
        FRAMES.fetch_add(1, Relaxed);
    }
    unsafe {
        let disp = oxivgl_sys::lv_display_get_default();
        assert!(!disp.is_null(), "no default display");
        oxivgl_sys::lv_display_add_event_cb(
            disp,
            Some(on_refr_ready),
            oxivgl_sys::lv_event_code_t_LV_EVENT_REFR_READY,
            core::ptr::null_mut(),
        );
    }
}

/// oxivgl's flush handoff, timed.
///
/// The block happens inside LVGL's C wait callback, where nothing outside the
/// pipeline can observe it — [`FlushSync`] being an application-supplied trait
/// is the entire reason `wait` is measurable at all. Wrapping rather than
/// reimplementing keeps the harness measuring the shipped path.
pub struct TimedFlushSync(&'static SemaphoreFlushSync);

impl TimedFlushSync {
    pub const fn new(inner: &'static SemaphoreFlushSync) -> Self {
        Self(inner)
    }
}

impl FlushSync for TimedFlushSync {
    fn wait(&self) {
        let t0 = esp_radio_rtos_driver::now();
        self.0.wait();
        WAIT_US.fetch_add((esp_radio_rtos_driver::now() - t0) as u32, Relaxed);
    }

    fn signal(&self) {
        self.0.signal();
    }
}

/// Microseconds spent with nothing ready to run, per core. Indexed by CPU
/// because the idle hook is installed for whichever core enters it — with the
/// scheduler running on both, a single counter would blend two cores into one
/// meaningless number.
pub static IDLE_US: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// Idle hook that accounts time instead of sleeping in `wait_for_interrupt`.
///
/// Runs only when no task is ready, so the time it accumulates *is* system idle.
/// Deltas above the cap are dropped: a long gap means the scheduler ran someone
/// else in between, which is busy time, not idle.
///
/// Note this trades the core's sleep for a spin, so it costs power — a
/// measurement build's bargain, and identical in every mode, so comparisons
/// between them stay valid. LVGL's own `% CPU` overlay is *not* a substitute: it
/// counts the flush wait as busy even when the CPU is free for other threads.
pub extern "C" fn idle_hook() -> ! {
    const CAP_US: u64 = 1_000;
    let core = esp_hal::system::Cpu::current() as usize;
    let mut last = esp_radio_rtos_driver::now();
    loop {
        let now = esp_radio_rtos_driver::now();
        let delta = now - last;
        last = now;
        if delta < CAP_US {
            IDLE_US[core].fetch_add(delta as u32, Relaxed);
        }
    }
}

/// Busy percent over an interval, from the idle counter. Saturates rather than
/// wrapping if the accounting drifts past the wall clock.
pub fn take_busy_pct(core: usize, interval_us: u32) -> u32 {
    let idle = IDLE_US[core].swap(0, Relaxed).min(interval_us);
    100 - (idle as u64 * 100 / interval_us.max(1) as u64) as u32
}

pub struct Latency {
    count: AtomicU32,
    sum_us: AtomicU32,
    max_us: AtomicU32,
    over_5ms: AtomicU32,
    over_20ms: AtomicU32,
}

/// One reporting interval's worth of latency, in microseconds.
#[derive(Default)]
pub struct LatencySnapshot {
    pub count: u32,
    pub mean_us: u32,
    pub max_us: u32,
    pub over_5ms: u32,
    pub over_20ms: u32,
}

impl Latency {
    const fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
            sum_us: AtomicU32::new(0),
            max_us: AtomicU32::new(0),
            over_5ms: AtomicU32::new(0),
            over_20ms: AtomicU32::new(0),
        }
    }

    pub fn record(&self, us: u32) {
        self.count.fetch_add(1, Relaxed);
        self.sum_us.fetch_add(us, Relaxed);
        self.max_us.fetch_max(us, Relaxed);
        if us > 5_000 {
            self.over_5ms.fetch_add(1, Relaxed);
        }
        if us > 20_000 {
            self.over_20ms.fetch_add(1, Relaxed);
        }
    }

    /// Read and reset. Not atomic as a group — a sample landing mid-read is
    /// counted in the next interval, which does not matter over 1 s windows.
    pub fn take(&self) -> LatencySnapshot {
        let count = self.count.swap(0, Relaxed);
        let sum = self.sum_us.swap(0, Relaxed);
        LatencySnapshot {
            count,
            mean_us: if count > 0 { sum / count } else { 0 },
            max_us: self.max_us.swap(0, Relaxed),
            over_5ms: self.over_5ms.swap(0, Relaxed),
            over_20ms: self.over_20ms.swap(0, Relaxed),
        }
    }
}

/// Which ceiling the pipeline is currently against, from the same numbers a
/// user would read. `wait` is time the render thread spent blocked on a flush:
/// high means it is waiting for the panel, zero means the panel is waiting for
/// it. This is the diagnostic the docs describe, applied to itself.
pub fn regime(draw_pct: u32, wait_pct: u32) -> &'static str {
    if wait_pct >= 15 {
        "DMA-bound"
    } else if draw_pct >= 30 {
        "CPU-bound"
    } else {
        "headroom"
    }
}

/// Read and reset the frame counters, returning `(frames, bytes, ops)`.
pub fn take_frames(prev_bytes: &mut u32, prev_ops: &mut u32) -> (u32, u32, u32) {
    let bytes = FLUSH_BYTES.load(Relaxed);
    let ops = FLUSH_OPS.load(Relaxed);
    let d_bytes = bytes.wrapping_sub(*prev_bytes);
    let d_ops = ops.wrapping_sub(*prev_ops);
    *prev_bytes = bytes;
    *prev_ops = ops;
    (FRAMES.swap(0, Relaxed), d_bytes, d_ops)
}
