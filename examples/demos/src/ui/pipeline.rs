// SPDX-License-Identifier: MIT OR Apache-2.0
//! LVGL display setup and the render→flush handoff, owned by this demo rather
//! than taken from `oxivgl::flush_pipeline`.
//!
//! The stock pipeline's `flush_wait_cb` parks the core in `waiti 0` until an
//! interrupt. That halts the scheduler for the whole transfer — 15-30 ms at
//! 40 MHz — so no other thread runs while LVGL waits, only ISRs. Here the render
//! thread blocks on an RTOS semaphore instead and the core stays available to
//! everyone else for the duration (#63).
//!
//! LVGL itself is still touched from exactly one thread: the flush side only
//! moves bytes, and `lv_display_flush_ready` is called from the render thread in
//! `wait_cb`. That is what keeps `LV_USE_OS LV_OS_NONE` correct.

use core::ffi::c_void;
use core::ptr::NonNull;
use core::sync::atomic::{
    AtomicPtr,
    Ordering::{Acquire, Relaxed, Release},
};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use esp_radio_rtos_driver::semaphore::{SemaphoreHandle, SemaphoreKind};
use oxivgl::display::LvglBuffers;
use oxivgl::driver::LvglDriver;
use oxivgl::flush_pipeline::{DisplayOutput, UiError};
use oxivgl_sys::*;

use super::metrics;

/// One dirty rectangle in flight.
pub struct DrawOp {
    data: &'static [u8],
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

// SAFETY: moved from the render thread to the flush side and never aliased —
// LVGL's `flushing` flag holds the buffer until `lv_display_flush_ready`.
unsafe impl Send for DrawOp {}

static DRAW: Channel<CriticalSectionRawMutex, DrawOp, 1> = Channel::new();
/// Raised once the flush side is running.
pub static READY: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// The display LVGL handed us, kept for `flush_ready` and the refresh timer.
static DISP: AtomicPtr<lv_display_t> = AtomicPtr::new(core::ptr::null_mut());
/// Completion semaphore, leaked at init so it outlives every user.
static SEM: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// Borrow the completion semaphore for one call.
fn with_sem<R>(f: impl FnOnce(&SemaphoreHandle) -> R) -> R {
    let ptr = NonNull::new(SEM.load(Acquire)).expect("flush semaphore not initialised");
    // SAFETY: `SEM` is set once in `init` from a leaked handle that is never
    // dropped, so the pointee outlives this borrow.
    f(unsafe { SemaphoreHandle::ref_from_ptr(&ptr) })
}

/// Hand a finished transfer back to the waiting render thread.
///
/// The give must match the flush side's context: an `InterruptExecutor` task
/// runs in interrupt context, where only the `_from_isr` form is legal.
fn signal_done() {
    with_sem(|s| {
        #[cfg(feature = "ui-flush-thread")]
        let ok = s.give();
        #[cfg(not(feature = "ui-flush-thread"))]
        let ok = s.try_give_from_isr(None);
        if !ok {
            log::error!("flush completion semaphore rejected the give");
        }
    });
}

/// LVGL flush callback — render thread, synchronous.
unsafe extern "C" fn flush_cb(disp: *mut lv_display_t, area: *const lv_area_t, px_map: *mut u8) {
    if disp.is_null() || area.is_null() || px_map.is_null() {
        log::error!("flush_cb: null argument");
        return;
    }
    // SAFETY: LVGL guarantees `area` is valid for the call.
    let a = unsafe { &*area };
    let w = (a.x2 - a.x1 + 1) as u16;
    let h = (a.y2 - a.y1 + 1) as u16;
    let len = w as usize * h as usize * 2;
    // SAFETY: LVGL owns `px_map` until `lv_display_flush_ready`, which only runs
    // after the flush side has consumed this slice.
    let data = unsafe { core::slice::from_raw_parts(px_map as *const u8, len) };

    let op = DrawOp { data, x: a.x1 as u16, y: a.y1 as u16, w, h };
    // Capacity 1 and LVGL waits for completion before the next flush, so a full
    // channel means the protocol was violated, not back-pressure.
    if DRAW.try_send(op).is_err() {
        log::error!("flush_cb: draw channel full");
    }
}

/// LVGL flush-wait callback — render thread, synchronous.
///
/// The blocking `take` is the point of this module: the thread leaves the run
/// queue and the core goes to whoever is ready next.
unsafe extern "C" fn wait_cb(disp: *mut lv_display_t) {
    let t0 = esp_radio_rtos_driver::now();
    with_sem(|s| s.take(None));
    metrics::WAIT_US.fetch_add((esp_radio_rtos_driver::now() - t0) as u32, Relaxed);
    // SAFETY: `disp` is LVGL's own pointer, valid for the display lifetime, and
    // this runs on the render thread — the only thread that touches LVGL.
    unsafe { lv_display_flush_ready(disp) };
}

/// Initialise LVGL and the display. **Must run on the render thread** — every
/// later LVGL call happens there too.
///
/// # Safety
/// Call exactly once, before any other LVGL use.
pub unsafe fn init<const BYTES: usize>(
    w: i32,
    h: i32,
    bufs: &'static mut LvglBuffers<BYTES>,
) -> LvglDriver {
    let sem = SemaphoreHandle::new(SemaphoreKind::Counting { max: 1, initial: 0 });
    SEM.store(sem.leak().as_ptr(), Release);

    let driver = LvglDriver::init(w, h);
    // SAFETY: `lv_init` ran above; the buffers are `'static`.
    unsafe {
        let buf1 = core::ptr::addr_of_mut!(bufs.buf1) as *mut c_void;
        let buf2 = core::ptr::addr_of_mut!(bufs.buf2) as *mut c_void;
        assert_eq!(buf1 as usize % 4, 0, "DMA buffer must be 4-byte aligned");

        let disp = lv_display_create(w, h);
        assert!(!disp.is_null(), "lv_display_create returned NULL");
        lv_display_set_color_format(disp, lv_color_format_t_LV_COLOR_FORMAT_RGB565_SWAPPED);
        lv_display_set_buffers(
            disp,
            buf1,
            buf2,
            BYTES as u32,
            lv_display_render_mode_t_LV_DISPLAY_RENDER_MODE_PARTIAL,
        );
        lv_display_set_flush_cb(disp, Some(flush_cb));
        lv_display_set_flush_wait_cb(disp, Some(wait_cb));
        DISP.store(disp, Release);
    }
    driver
}

/// Set LVGL's refresh period at runtime, so `lv_conf.h` — shared with the other
/// demo — keeps its default. The stock 32 ms caps the frame rate at 31 fps
/// before any render cost is counted.
pub fn set_refresh_period(ms: u32) {
    let disp = DISP.load(Acquire);
    assert!(!disp.is_null(), "set_refresh_period before init");
    // SAFETY: `disp` came from `lv_display_create`; called on the render thread.
    unsafe {
        let timer = lv_display_get_refr_timer(disp);
        assert!(!timer.is_null(), "display has no refresh timer");
        lv_timer_set_period(timer, ms);
    }
}

/// Drain dirty rectangles to the panel. Runs on whichever context the demo's
/// mode selected — an interrupt executor, or its own thread.
pub async fn flush_worker(mut out: impl DisplayOutput) -> ! {
    READY.signal(());
    loop {
        let op = DRAW.receive().await;
        if let Err(UiError::Display) = out.show_raw_data(op.x, op.y, op.w, op.h, op.data).await {
            log::error!("show_raw_data failed");
        }
        metrics::FLUSH_BYTES.fetch_add(op.data.len() as u32, Relaxed);
        metrics::FLUSH_OPS.fetch_add(1, Relaxed);
        signal_done();
    }
}
