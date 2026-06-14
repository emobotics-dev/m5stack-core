// SPDX-License-Identifier: MIT OR Apache-2.0
//! Front-panel input → LVGL indev, chosen per the board's `io::input_caps()`:
//!
//! - **Fire27** (`InputCaps::Keypad`): the three buttons map to LVGL nav keys
//!   (PREV/ENTER/NEXT) posted to a [`KeypadState`] the render loop reads — the
//!   I4 keypad adapter fed by the unified `ButtonEvent`.
//! - **CoreS3** (`InputCaps::Pointer`): the FT6336U is driven as a real **LVGL
//!   POINTER** (oxivgl 0.5) — widgets are tapped by coordinate, not faked as
//!   nav keys (#32 I3). An async poll task bridges the I2C read (which can't run
//!   in LVGL's sync indev callback) into a lock-free [`PointerState`].

// --- Fire27: keypad path -------------------------------------------------

#[cfg(feature = "fire27")]
pub use keypad::{KEYPAD, input_task, wake};

#[cfg(feature = "fire27")]
mod keypad {
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::signal::Signal;
    use oxivgl::enums::Key;
    use oxivgl::indev::KeypadState;

    use crate::board::{ButtonId, Input};

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
