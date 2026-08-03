// SPDX-License-Identifier: MIT OR Apache-2.0
//! LVGL's OS port (`LV_OS_CUSTOM`) over esp-rtos.
//!
//! LVGL ships ports for FreeRTOS, pthreads, CMSIS and others but not esp-rtos,
//! so with `LV_USE_OS LV_OS_NONE` it is told the system is single-threaded and
//! `LV_DRAW_SW_DRAW_UNIT_CNT` is stuck at 1. Since the render cost is per draw
//! *task* (#63), parallel draw units are the structural lever, and they need a
//! real OS underneath.
//!
//! Types are in `conf/m5_lv_os.h`; LVGL declares the functions itself.
//!
//! ## Priority is clamped, deliberately
//!
//! LVGL asks for `LV_THREAD_PRIO_HIGH` for its draw threads. Honouring that
//! literally would put rasterisation above the latency-sensitive work the whole
//! scheduling model exists to protect. Every LVGL thread is pinned to
//! [`sched::PRIO_RENDER`] instead — LVGL's own scale is mapped onto one rung,
//! because within LVGL the relative order does not matter, and against the rest
//! of the system it very much does.

use core::ffi::{CStr, c_char, c_void};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use esp_radio_rtos_driver::semaphore::{SemaphoreHandle, SemaphoreKind};

use crate::common::sched;

/// `lv_result_t`: 0 invalid, 1 ok.
const LV_RESULT_INVALID: u32 = 0;
const LV_RESULT_OK: u32 = 1;

#[repr(C)]
pub struct LvThread {
    task: *mut c_void,
}
#[repr(C)]
pub struct LvMutex {
    sem: *mut c_void,
}
#[repr(C)]
pub struct LvThreadSync {
    sem: *mut c_void,
}

/// LVGL threads follow the render thread's core. Spreading them across cores is
/// tempting — two draw units on one core only time-slice — but it silently
/// moves rasterisation onto the core the UI is supposed to be staying off, and
/// LVGL spawns a draw thread even at DRAW_UNIT_CNT=1. Co-locating keeps the
/// placement the application chose.
static THREADS: AtomicU32 = AtomicU32::new(0);

fn make_sem(kind: SemaphoreKind) -> *mut c_void {
    SemaphoreHandle::new(kind).leak().as_ptr() as *mut c_void
}

/// Borrow a leaked semaphore for one call.
fn with_sem<R>(raw: *mut c_void, f: impl FnOnce(&SemaphoreHandle) -> R) -> Option<R> {
    let ptr = NonNull::new(raw as *mut ())?;
    // SAFETY: every pointer stored in these handles came from `leak()` and is
    // never freed, so the pointee outlives the borrow.
    Some(f(unsafe { SemaphoreHandle::ref_from_ptr(&ptr) }))
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_thread_init(
    thread: *mut LvThread,
    name: *const c_char,
    _prio: u32,
    callback: Option<extern "C" fn(*mut c_void)>,
    stack_size: usize,
    user_data: *mut c_void,
) -> u32 {
    let (Some(thread), Some(callback)) = (unsafe { thread.as_mut() }, callback) else {
        return LV_RESULT_INVALID;
    };
    // SAFETY: LVGL passes a static string literal.
    let name = if name.is_null() {
        "lvgl"
    } else {
        unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("lvgl")
    };
    THREADS.fetch_add(1, Relaxed);
    let core = sched::RENDER_CORE;
    // SAFETY: LVGL's render thread callback loops forever; the task is never
    // deleted while LVGL is running.
    unsafe {
        let task = esp_radio_rtos_driver::task_create(
            name,
            callback,
            user_data,
            sched::PRIO_RENDER,
            Some(core),
            stack_size,
        );
        thread.task = task.as_ptr() as *mut c_void;
    }
    LV_RESULT_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_thread_delete(thread: *mut LvThread) -> u32 {
    let Some(thread) = (unsafe { thread.as_mut() }) else {
        return LV_RESULT_INVALID;
    };
    let Some(task) = NonNull::new(thread.task as *mut ()) else {
        return LV_RESULT_INVALID;
    };
    // SAFETY: `task` was returned by `task_create` and is deleted at most once —
    // LVGL calls this only from its own teardown.
    unsafe { esp_radio_rtos_driver::schedule_task_deletion(Some(task.cast())) };
    thread.task = core::ptr::null_mut();
    LV_RESULT_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_mutex_init(mutex: *mut LvMutex) -> u32 {
    let Some(mutex) = (unsafe { mutex.as_mut() }) else {
        return LV_RESULT_INVALID;
    };
    mutex.sem = make_sem(SemaphoreKind::Mutex);
    LV_RESULT_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_mutex_lock(mutex: *mut LvMutex) -> u32 {
    let Some(mutex) = (unsafe { mutex.as_ref() }) else {
        return LV_RESULT_INVALID;
    };
    match with_sem(mutex.sem, |s| s.take(None)) {
        Some(true) => LV_RESULT_OK,
        _ => LV_RESULT_INVALID,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_mutex_lock_isr(mutex: *mut LvMutex) -> u32 {
    let Some(mutex) = (unsafe { mutex.as_ref() }) else {
        return LV_RESULT_INVALID;
    };
    match with_sem(mutex.sem, |s| s.try_take_from_isr(None)) {
        Some(true) => LV_RESULT_OK,
        _ => LV_RESULT_INVALID,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_mutex_unlock(mutex: *mut LvMutex) -> u32 {
    let Some(mutex) = (unsafe { mutex.as_ref() }) else {
        return LV_RESULT_INVALID;
    };
    match with_sem(mutex.sem, |s| s.give()) {
        Some(true) => LV_RESULT_OK,
        _ => LV_RESULT_INVALID,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_mutex_delete(mutex: *mut LvMutex) -> u32 {
    let Some(mutex) = (unsafe { mutex.as_mut() }) else {
        return LV_RESULT_INVALID;
    };
    // Leaked on purpose: LVGL deletes its general mutex at teardown, which this
    // firmware never reaches, and freeing a semaphore another thread may still
    // hold is the worse failure.
    mutex.sem = core::ptr::null_mut();
    LV_RESULT_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_thread_sync_init(sync: *mut LvThreadSync) -> u32 {
    let Some(sync) = (unsafe { sync.as_mut() }) else {
        return LV_RESULT_INVALID;
    };
    sync.sem = make_sem(SemaphoreKind::Counting { max: 1, initial: 0 });
    LV_RESULT_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_thread_sync_wait(sync: *mut LvThreadSync) -> u32 {
    let Some(sync) = (unsafe { sync.as_ref() }) else {
        return LV_RESULT_INVALID;
    };
    match with_sem(sync.sem, |s| s.take(None)) {
        Some(true) => LV_RESULT_OK,
        _ => LV_RESULT_INVALID,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_thread_sync_signal(sync: *mut LvThreadSync) -> u32 {
    let Some(sync) = (unsafe { sync.as_ref() }) else {
        return LV_RESULT_INVALID;
    };
    // A signal with the semaphore already full is not an error: the waiter has
    // simply not consumed the previous one yet, and the count saturates at 1.
    with_sem(sync.sem, |s| s.give());
    LV_RESULT_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_thread_sync_signal_isr(sync: *mut LvThreadSync) -> u32 {
    let Some(sync) = (unsafe { sync.as_ref() }) else {
        return LV_RESULT_INVALID;
    };
    with_sem(sync.sem, |s| s.try_give_from_isr(None));
    LV_RESULT_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn lv_thread_sync_delete(sync: *mut LvThreadSync) -> u32 {
    let Some(sync) = (unsafe { sync.as_mut() }) else {
        return LV_RESULT_INVALID;
    };
    sync.sem = core::ptr::null_mut();
    LV_RESULT_OK
}
