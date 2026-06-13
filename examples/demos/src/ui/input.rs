// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fire27 front-panel buttons → LVGL keypad input device.
//!
//! Verbatim per-edge keypad latch (NOT `io::buttons`): LVGL's keypad wants an
//! immediate key code on the press edge, which differs from `async-button`'s
//! debounced short/long semantics. The CoreS3 is touch-only and registers no
//! keypad indev.

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{AnyPin, Input, InputConfig};
use oxivgl::enums::Key;

/// Pending LVGL key code written by the button tasks and consumed by the LVGL
/// read callback. `0` means "no pending key". Single-core ESP32: `Relaxed`
/// ordering is sufficient.
static KEY_PENDING: AtomicU32 = AtomicU32::new(0);

/// One task per button. Awaits a press edge, latches the LVGL key code, then
/// debounces the release so a single press maps to a single key event.
#[embassy_executor::task(pool_size = 3)]
async fn button_task(mut pin: Input<'static>, key_code: u32) -> ! {
    // Buttons are active-low (external pull-ups on GPIO34-39). Respond on the
    // PRESS edge for minimal latency — interrupt-driven, no polling.
    const DEBOUNCE: Duration = Duration::from_millis(15);
    loop {
        pin.wait_for_falling_edge().await;
        KEY_PENDING.store(key_code, Ordering::Relaxed);
        Timer::after(DEBOUNCE).await; // settle press bounce
        pin.wait_for_rising_edge().await; // wait for release
        Timer::after(DEBOUNCE).await; // settle release bounce
    }
}

/// LVGL keypad read callback, invoked by LVGL on every timer tick.
///
/// # Safety
/// Called from the LVGL task only (single-core ESP32). `data` is a non-null
/// pointer LVGL owns for the duration of this callback.
unsafe extern "C" fn keypad_read_cb(
    _indev: *mut oxivgl_sys::lv_indev_t,
    data: *mut oxivgl_sys::lv_indev_data_t,
) {
    let key = KEY_PENDING.swap(0, Ordering::Relaxed);
    // SAFETY: `data` is non-null and exclusively owned by LVGL here.
    unsafe {
        if key != 0 {
            (*data).key = key;
            (*data).state = oxivgl_sys::lv_indev_state_t_LV_INDEV_STATE_PRESSED;
        } else {
            (*data).state = oxivgl_sys::lv_indev_state_t_LV_INDEV_STATE_RELEASED;
        }
    }
}

/// Register the LVGL keypad input device backed by the three hardware buttons.
///
/// Must be called after `lv_init()` (i.e. from inside `View::create`, which
/// `run_app` invokes after driver init) and before any focusable widget needs
/// keypad routing.
pub fn register_keypad_indev() {
    // SAFETY: `lv_indev_create`/`set_type`/`set_read_cb` run after `lv_init()`
    // (guaranteed by `run_app` calling `create`). The indev pointer is checked
    // non-null and `keypad_read_cb` has the correct signature for a KEYPAD indev.
    unsafe {
        let indev = oxivgl_sys::lv_indev_create();
        assert!(!indev.is_null(), "lv_indev_create returned NULL");
        oxivgl_sys::lv_indev_set_type(indev, oxivgl_sys::lv_indev_type_t_LV_INDEV_TYPE_KEYPAD);
        oxivgl_sys::lv_indev_set_read_cb(indev, Some(keypad_read_cb));
    }
}

/// Spawn the three front-panel button tasks (A=PREV, B=ENTER, C=NEXT). The pins
/// come from the BSP's `ButtonResources` (GPIO39/38/37). GPIO34-39 are
/// input-only; the Fire27 has external pull-ups, so a bare `InputConfig` is
/// correct.
pub fn spawn(spawner: Spawner, left: AnyPin<'static>, center: AnyPin<'static>, right: AnyPin<'static>) {
    let btn_a = Input::new(left, InputConfig::default()); // A — PREV
    let btn_b = Input::new(center, InputConfig::default()); // B — ENTER
    let btn_c = Input::new(right, InputConfig::default()); // C — NEXT
    // pool_size = 3 fits all three buttons; exhaustion here is a startup bug.
    spawner.spawn(button_task(btn_a, Key::PREV.0).expect("spawn button A"));
    spawner.spawn(button_task(btn_b, Key::ENTER.0).expect("spawn button B"));
    spawner.spawn(button_task(btn_c, Key::NEXT.0).expect("spawn button C"));
}
