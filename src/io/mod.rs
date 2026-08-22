// SPDX-License-Identifier: MIT OR Apache-2.0

/// What input model a board exposes, so a consumer can install the matching
/// LVGL indev (keypad vs pointer) instead of assuming three buttons (#32 I2).
/// This is a *capability descriptor* only — it carries no app vocabulary
/// (no nav/semantic meaning); see [`buttons::ButtonEvent`] for the I1 event
/// contract and [`input_caps`] to query the running board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputCaps {
    /// Discrete buttons (Fire27: three front-panel buttons). Drive an LVGL
    /// KEYPAD/ENCODER indev from the [`buttons::ButtonEvent`] stream.
    Keypad { buttons: u8 },
    /// A touch panel of the given pixel size (CoreS3: FT6336U). Drive an LVGL
    /// POINTER indev from `driver::ft6336u::read_touch`, or the zone-emulated
    /// [`touch_buttons::TouchButtons`] keypad stream.
    Pointer { width: u16, height: u16 },
}

/// The running board's input capability (#32 I2). Fire27 reports a 3-button
/// keypad; CoreS3 reports its touch-panel size. Lets a view install the right
/// indev without hardcoding the board.
pub const fn input_caps() -> InputCaps {
    #[cfg(feature = "fire27")]
    {
        InputCaps::Keypad { buttons: 3 }
    }
    #[cfg(feature = "cores3")]
    {
        InputCaps::Pointer {
            width: crate::board::SCREEN_W,
            height: crate::board::SCREEN_H,
        }
    }
}

pub mod buttons;
pub mod console;
pub mod ow_temp;
pub mod pps;
pub mod rpm;
#[cfg(feature = "serial-cmd")]
pub mod serial_cmd;
pub mod shared_i2c;
pub mod touch_buttons;
pub mod watchdog;
