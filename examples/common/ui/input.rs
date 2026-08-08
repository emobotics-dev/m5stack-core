// SPDX-License-Identifier: MIT OR Apache-2.0
//! Front-panel input → LVGL indev(s).
//!
//! - **Keypad (both boards)**: the unified [`ButtonEvent`](crate::common::board::ButtonEvent)
//!   — Fire27's physical buttons or CoreS3's bottom-strip touch *keys* (the BSP
//!   `TouchButtons`, with multi-tap / long-press) — maps to LVGL nav keys
//!   (PREV/ENTER/NEXT). The bottom keys deliberately stay on the **button API**,
//!   not the pointer, because the consumer app's primary input is those keys.
//! - **Pointer (CoreS3, additionally)**: the FT6336U is *also* driven as a real
//!   **LVGL POINTER** (oxivgl 0.5) so on-screen widgets can be tapped by
//!   coordinate (#32 I3). An async poll task bridges the I2C read (which can't
//!   run in LVGL's sync indev callback) into a lock-free [`PointerState`].
//!
//! So CoreS3 has both: tap a widget directly, or navigate with the bottom keys.

// --- Keypad path (both boards) -------------------------------------------

pub use keypad::{KEYPAD, input_task, wake};

mod keypad {
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::signal::Signal;
    use oxivgl::enums::Key;
    use oxivgl::indev::KeypadState;

    use crate::common::board::{ButtonId, Input};

    /// LVGL keypad state: fed by [`input_task`], read by the render loop's
    /// keypad indev. `'static` because LVGL stores a pointer to it.
    pub static KEYPAD: KeypadState = KeypadState::new();

    static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

    /// Wake future for `run_app_nav_keypad_events` to race against its inter-tick
    /// sleep — resolves whenever [`input_task`] has posted a key.
    pub async fn wake() {
        WAKE.wait().await;
    }

    /// Decode the front-panel buttons into LVGL keys and post them:
    /// Left→PREV / Center→ENTER / Right→NEXT.
    #[embassy_executor::task]
    pub async fn input_task(mut input: Input) {
        loop {
            let ev = input.next_event().await;
            let key = match ev.id {
                ButtonId::Left => Key::PREV,
                ButtonId::Center => Key::ENTER,
                ButtonId::Right => Key::NEXT,
            };
            KEYPAD.send(key);
            WAKE.signal(());
        }
    }
}

// --- CoreS3: touchscreen POINTER path ------------------------------------

#[cfg(feature = "cores3")]
pub use pointer::{POINTER, touch_poll_task};

#[cfg(feature = "cores3")]
mod pointer {
    use embassy_time::{Duration, Timer};
    use m5stack_core::driver::ft6336u;
    use m5stack_core::io::shared_i2c::SharedI2cBus;
    use oxivgl::indev::PointerState;

    /// Latest touch for the LVGL POINTER indev, fed by [`touch_poll_task`].
    /// `'static` because LVGL stores a pointer to it.
    pub static POINTER: PointerState = PointerState::new();

    /// Poll the FT6336U over the shared I2C bus (~50 Hz) and publish the latest
    /// touch into [`POINTER`]. Bridges the **async** I2C read to the LVGL
    /// POINTER indev's **sync** read callback — the indev can't await, so this
    /// task owns the I2C and the indev only reads the lock-free cell.
    #[embassy_executor::task]
    pub async fn touch_poll_task(i2c: &'static SharedI2cBus) {
        loop {
            match ft6336u::read_touch(i2c).await {
                Ok(Some((x, y))) => POINTER.touch(x, y),
                _ => POINTER.release(),
            }
            Timer::after(Duration::from_millis(20)).await;
        }
    }
}
