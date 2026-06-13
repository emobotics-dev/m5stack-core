// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unified front-panel button events.
//!
//! Both input flavours of the M5Stack Core family emit the same
//! [`ButtonEvent`]: the Fire27's three physical buttons ([`Buttons`], feature
//! `buttons`) and the CoreS3's touch-strip emulation
//! ([`crate::io::touch_buttons::TouchButtons`]). An application maps these to
//! its own events in one place, identically for both boards.

use esp_hal::gpio::AnyPin;

/// Which of the three front-panel positions fired. On the Fire27 these are
/// buttons A/B/C; on the CoreS3, the left/center/right touch zones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonId {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonAction {
    /// Short press(es); the count is the number of consecutive taps.
    Short(usize),
    Long,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonEvent {
    pub id: ButtonId,
    pub action: ButtonAction,
}

/// The three front-panel button pins (Fire27: A=GPIO39, B=GPIO38, C=GPIO37,
/// active-low). Wired by [`crate::board::fire27::Board::split`].
pub struct ButtonResources<'a> {
    pub left: AnyPin<'a>,
    pub center: AnyPin<'a>,
    pub right: AnyPin<'a>,
}

#[cfg(feature = "buttons")]
mod physical {
    use async_button::{Button, ButtonConfig, ButtonEvent as AsyncButtonEvent};
    use embassy_futures::select::{Either3, select3};
    use esp_hal::gpio::{Input, InputConfig, Pull};

    use super::{ButtonAction, ButtonEvent, ButtonId, ButtonResources};

    impl ButtonResources<'static> {
        /// Wrap the pins in debounced [`async_button`] drivers (pull-ups on,
        /// default debounce/long-press timings).
        pub fn into_buttons(self) -> Buttons<'static> {
            let make = |pin| {
                Button::new(
                    Input::new(pin, InputConfig::default().with_pull(Pull::Up)),
                    ButtonConfig::default(),
                )
            };
            Buttons {
                left: make(self.left),
                center: make(self.center),
                right: make(self.right),
            }
        }
    }

    /// The three debounced front-panel buttons.
    pub struct Buttons<'a> {
        left: Button<Input<'a>>,
        center: Button<Input<'a>>,
        right: Button<Input<'a>>,
    }

    impl Buttons<'_> {
        /// Wait for the next debounced event on any of the three buttons.
        /// No polling — the underlying driver awaits pin edges.
        pub async fn next_event(&mut self) -> ButtonEvent {
            let (id, event) = match select3(
                self.left.update(),
                self.center.update(),
                self.right.update(),
            )
            .await
            {
                Either3::First(e) => (ButtonId::Left, e),
                Either3::Second(e) => (ButtonId::Center, e),
                Either3::Third(e) => (ButtonId::Right, e),
            };
            let action = match event {
                AsyncButtonEvent::ShortPress { count } => ButtonAction::Short(count),
                AsyncButtonEvent::LongPress => ButtonAction::Long,
            };
            ButtonEvent { id, action }
        }
    }
}

#[cfg(feature = "buttons")]
pub use physical::Buttons;
