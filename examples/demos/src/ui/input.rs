// SPDX-License-Identifier: MIT OR Apache-2.0
//! Front-panel input → LVGL keypad.
//!
//! Maps the BSP's unified [`ButtonEvent`](crate::board::ButtonEvent) — Fire27
//! physical buttons or CoreS3 touch zones, the *same* event on both — to LVGL
//! navigation keys, posting them to a [`KeypadState`] that
//! `run_app_nav_keypad_events` reads. So one input task drives the focusable
//! LVGL widgets identically on both boards (this is exactly #32's I4: a generic
//! KEYPAD adapter fed by the unified input, with no app vocabulary).

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use oxivgl::enums::Key;
use oxivgl::indev::KeypadState;

use crate::board::{ButtonId, Input};

/// LVGL keypad state: fed by [`input_task`], read by the render loop's keypad
/// indev. `'static` because LVGL stores a pointer to it.
pub static KEYPAD: KeypadState = KeypadState::new();

/// Signalled after each key send so the event-mode render loop reads it without
/// waiting for the next periodic tick.
static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Wake future for `run_app_nav_keypad_events` to race against its inter-tick
/// sleep — resolves whenever [`input_task`] has posted a key.
pub async fn wake() {
    WAKE.wait().await;
}

/// Decode the unified front-panel events into LVGL keys and post them:
/// Left→PREV / Center→ENTER / Right→NEXT (`Short` and `Long` alike — this demo
/// only navigates). Identical on Fire27 buttons and CoreS3 touch zones.
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
