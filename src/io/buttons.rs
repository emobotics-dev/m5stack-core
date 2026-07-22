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

/// A generic, positional input event — the #32 I1/I6 contract: it carries only
/// *where* (`id`) and *what kind* (`action`), never app semantics (no
/// Inc/Dec/Ok, no nav). Both the Fire27 physical buttons and the CoreS3 touch
/// zones emit this same type, so a consumer's input router maps it to meaning;
/// the BSP never does. Do not add app-vocabulary variants here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonEvent {
    pub id: ButtonId,
    pub action: ButtonAction,
}

/// Press-decoding timing shared by **both** front-panel drivers — the Fire27
/// physical buttons (`ButtonResources::into_buttons_with_timing`, feature
/// `buttons`) and the CoreS3 touch strip
/// ([`TouchButtonsConfig::with_timing`](crate::io::touch_buttons::TouchButtonsConfig::with_timing)).
/// Milliseconds (the touch driver's native unit; the Fire27 `async-button`
/// `Duration`s are built from these).
///
/// The knob that matters for latency is `multi_tap_ms`: both drivers count
/// consecutive taps, which delays *every* single press by that window. Set it to
/// `0` (see [`immediate_single_press`](Self::immediate_single_press)) for
/// latency-sensitive input like an encoder — a downstream accumulator can still
/// sum immediate `Short(1)` events into multi-step (#58).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonTiming {
    /// Hold time before a press becomes [`ButtonAction::Long`].
    pub long_press_ms: u64,
    /// Max gap between consecutive taps still counted as one multi-tap sequence.
    /// `0` disables counting: each press emits [`Short(1)`](ButtonAction::Short)
    /// as soon as it is debounced, with no counting delay.
    pub multi_tap_ms: u64,
}

impl ButtonTiming {
    /// Count multi-taps (the historical behaviour): a single press is delayed by
    /// the `multi_tap_ms` window so consecutive taps can be counted. 500 ms
    /// long-press, 300 ms multi-tap window.
    pub const fn multi_tap() -> Self {
        Self { long_press_ms: 500, multi_tap_ms: 300 }
    }

    /// Report every press immediately — multi-tap counting disabled
    /// (`multi_tap_ms = 0`). For latency-sensitive input (e.g. an encoder). Keeps
    /// the [`multi_tap`](Self::multi_tap) long-press threshold.
    pub const fn immediate_single_press() -> Self {
        Self { long_press_ms: Self::multi_tap().long_press_ms, multi_tap_ms: 0 }
    }
}

/// The unified opt-in baseline is [`multi_tap`](Self::multi_tap). Note the legacy
/// entry points (`ButtonResources::into_buttons`,
/// [`TouchButtons::new`](crate::io::touch_buttons::TouchButtons::new) with
/// [`TouchButtonsConfig::default`](crate::io::touch_buttons::TouchButtonsConfig::default))
/// keep each driver's *original* per-board timings for backward compatibility;
/// this default is only used where a `ButtonTiming` is taken explicitly.
impl Default for ButtonTiming {
    fn default() -> Self {
        Self::multi_tap()
    }
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
    use async_button::{Button, ButtonConfig, ButtonEvent as AsyncButtonEvent, Mode};
    use embassy_futures::select::{Either3, select3};
    use embassy_time_04::Duration;
    use esp_hal::gpio::{Input, InputConfig, Pull};

    use super::{ButtonAction, ButtonEvent, ButtonId, ButtonResources, ButtonTiming};

    /// Map the cross-driver [`ButtonTiming`] onto `async-button`'s [`ButtonConfig`]:
    /// `multi_tap_ms → double_click`, `long_press_ms → long_press`. Debounce keeps
    /// async-button's default; `mode` is [`Mode::PullUp`] — the BSP owns the
    /// active-low + pull-up wiring, the caller owns only the timings. The
    /// `Duration`s are `embassy-time` 0.4 (async-button's pin), built here from ms.
    fn config_from_timing(timing: ButtonTiming) -> ButtonConfig {
        ButtonConfig {
            double_click: Duration::from_millis(timing.multi_tap_ms),
            long_press: Duration::from_millis(timing.long_press_ms),
            mode: Mode::PullUp,
            ..ButtonConfig::default()
        }
    }

    impl ButtonResources<'static> {
        /// Wrap the pins in debounced [`async_button`] drivers with async-button's
        /// default timings (pull-ups on). A single press is delayed by the multi-tap
        /// window (~350 ms) so consecutive taps can be counted; for a different
        /// window — or immediate single-press — use
        /// [`into_buttons_with_timing`](Self::into_buttons_with_timing).
        pub fn into_buttons(self) -> Buttons<'static> {
            self.make_buttons(ButtonConfig::default())
        }

        /// Like [`into_buttons`](Self::into_buttons) but driven by the cross-driver
        /// [`ButtonTiming`] — the same type and presets the CoreS3 touch strip takes
        /// ([`TouchButtonsConfig::with_timing`](crate::io::touch_buttons::TouchButtonsConfig::with_timing)).
        /// Pass [`ButtonTiming::immediate_single_press`] to trade the multi-tap
        /// counting delay for instant response, or a custom window / long-press.
        pub fn into_buttons_with_timing(self, timing: ButtonTiming) -> Buttons<'static> {
            self.make_buttons(config_from_timing(timing))
        }

        fn make_buttons(self, config: ButtonConfig) -> Buttons<'static> {
            let make = |pin| {
                Button::new(
                    Input::new(pin, InputConfig::default().with_pull(Pull::Up)),
                    config,
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
