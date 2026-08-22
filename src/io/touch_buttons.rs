// SPDX-License-Identifier: MIT OR Apache-2.0
//! Touch→button emulation for the CoreS3 (FT6336U).
//!
//! The CoreS3 has no physical front-panel buttons; a strip of the touch panel
//! is split into three zones emulating the classic M5Stack Left/Center/Right
//! buttons. A small state machine detects short taps (with multi-tap
//! counting) and long presses and emits the same
//! [`ButtonEvent`](crate::io::buttons::ButtonEvent) as the Fire27's physical
//! buttons ([`crate::io::buttons`]), so the application maps input events in
//! one place for both boards.

use embassy_time::{Duration, Instant, Timer};

use crate::driver::ft6336u;
use crate::io::buttons::{ButtonAction, ButtonEvent, ButtonId, ButtonTiming};
use crate::io::shared_i2c::SharedI2cBus;

pub struct TouchButtonsConfig {
    /// Touch-poll interval.
    pub poll_ms: u64,
    /// Hold time before a press becomes [`ButtonAction::Long`].
    pub long_press_ms: u64,
    /// Max gap between taps that still counts as a multi-tap. Set to `0` to
    /// disable multi-tap counting entirely: each tap emits `Short(1)` on the next
    /// poll after release (~`poll_ms`), with no counting delay — for
    /// latency-sensitive input like an encoder (#58). See
    /// [`with_timing`](Self::with_timing) /
    /// [`ButtonTiming::immediate_single_press`] for the cross-driver preset.
    pub multi_tap_ms: u64,
    /// Zone strip: touches above this Y are ignored.
    pub zone_y_min: u16,
    /// X below this is [`ButtonId::Left`].
    pub zone_x_left: u16,
    /// X below this (and ≥ `zone_x_left`) is [`ButtonId::Center`]; above,
    /// [`ButtonId::Right`].
    pub zone_x_right: u16,
}

/// Bottom 40 px strip (y ≥ 200) split into thirds.
impl Default for TouchButtonsConfig {
    fn default() -> Self {
        Self {
            poll_ms: 20,
            long_press_ms: 500,
            multi_tap_ms: 300,
            zone_y_min: 200,
            zone_x_left: 107,
            zone_x_right: 213,
        }
    }
}

impl TouchButtonsConfig {
    /// [`Default`] geometry/poll, but with the press-decoding timings taken from
    /// the cross-driver [`ButtonTiming`] — the same type and presets the Fire27
    /// physical buttons take (`ButtonResources::into_buttons_with_timing`, feature
    /// `buttons`). Use [`ButtonTiming::immediate_single_press`] for instant
    /// single-press input (an encoder).
    pub fn with_timing(timing: ButtonTiming) -> Self {
        Self {
            long_press_ms: timing.long_press_ms,
            multi_tap_ms: timing.multi_tap_ms,
            ..Self::default()
        }
    }
}

/// The emulated three-button strip. Construct once, then await
/// [`next_event`](Self::next_event) in a loop.
pub struct TouchButtons {
    i2c: &'static SharedI2cBus,
    config: TouchButtonsConfig,
    /// FT6336U probed and answering (it may need AXP2101 power-up time).
    probed: bool,
    // press state
    pressed: Option<ButtonId>,
    press_start: Instant,
    long_fired: bool,
    // multi-tap state
    tap_zone: Option<ButtonId>,
    tap_count: usize,
    last_release: Instant,
}

impl TouchButtons {
    /// Construct with the cross-driver [`ButtonTiming`] (default geometry/poll) —
    /// the touch-side counterpart of `ButtonResources::into_buttons_with_timing`
    /// (feature `buttons`). Shorthand for
    /// `new(i2c, TouchButtonsConfig::with_timing(timing))`.
    pub fn with_timing(i2c: &'static SharedI2cBus, timing: ButtonTiming) -> Self {
        Self::new(i2c, TouchButtonsConfig::with_timing(timing))
    }

    pub fn new(i2c: &'static SharedI2cBus, config: TouchButtonsConfig) -> Self {
        Self {
            i2c,
            config,
            probed: false,
            pressed: None,
            press_start: Instant::now(),
            long_fired: false,
            tap_zone: None,
            tap_count: 0,
            last_release: Instant::now(),
        }
    }

    fn classify(&self, x: u16, y: u16) -> Option<ButtonId> {
        if y < self.config.zone_y_min {
            return None;
        }
        Some(if x < self.config.zone_x_left {
            ButtonId::Left
        } else if x < self.config.zone_x_right {
            ButtonId::Center
        } else {
            ButtonId::Right
        })
    }

    /// Wait for the FT6336U to appear on the bus (first call only — the
    /// controller may need AXP2101 power-up time after a cold boot).
    async fn probe(&mut self) {
        let mut attempts = 0u32;
        loop {
            match ft6336u::read_touch(self.i2c).await {
                Ok(_) => {
                    info!("FT6336U found at 0x{:02X}", ft6336u::ADDR);
                    self.probed = true;
                    return;
                }
                Err(e) => {
                    attempts += 1;
                    if attempts <= 3 || attempts % 50 == 0 {
                        warn!("FT6336U probe #{}: {:?}", attempts, e);
                    }
                }
            }
            Timer::after(Duration::from_millis(100)).await;
        }
    }

    /// Poll the touch controller until the state machine produces the next
    /// [`ButtonEvent`]. Press state persists across calls, so awaiting this
    /// in a loop loses no events.
    pub async fn next_event(&mut self) -> ButtonEvent {
        if !self.probed {
            self.probe().await;
        }

        loop {
            Timer::after(Duration::from_millis(self.config.poll_ms)).await;

            let touch = match ft6336u::read_touch(self.i2c).await {
                Ok(t) => t,
                Err(e) => {
                    warn!("touch I2C err: {:?}", e);
                    continue;
                }
            };
            let cur_zone = touch.and_then(|(x, y)| self.classify(x, y));

            match (self.pressed, cur_zone) {
                // No touch was active, still no touch: flush a pending
                // multi-tap once the tap window has passed.
                (None, None) => {
                    if self.tap_count > 0
                        && self.last_release.elapsed()
                            > Duration::from_millis(self.config.multi_tap_ms)
                    {
                        let id = self.tap_zone.take().unwrap();
                        let count = core::mem::take(&mut self.tap_count);
                        return ButtonEvent {
                            id,
                            action: ButtonAction::Short(count),
                        };
                    }
                }
                // New touch down in a zone.
                (None, Some(zone)) => {
                    self.pressed = Some(zone);
                    self.press_start = Instant::now();
                    self.long_fired = false;
                }
                // Touch held.
                (Some(zone), Some(_cur)) => {
                    if !self.long_fired
                        && self.press_start.elapsed()
                            > Duration::from_millis(self.config.long_press_ms)
                    {
                        self.long_fired = true;
                        // A long press cancels any pending multi-tap.
                        self.tap_count = 0;
                        self.tap_zone = None;
                        return ButtonEvent {
                            id: zone,
                            action: ButtonAction::Long,
                        };
                    }
                }
                // Touch released.
                (Some(zone), None) => {
                    self.pressed = None;
                    if !self.long_fired {
                        if self.tap_zone == Some(zone) {
                            // Same zone — accumulate the multi-tap.
                            self.tap_count += 1;
                            self.last_release = Instant::now();
                        } else {
                            // Different zone or first tap — flush the old
                            // zone's taps (if any), start counting the new.
                            let pending = self
                                .tap_zone
                                .replace(zone)
                                .filter(|_| self.tap_count > 0)
                                .map(|old| ButtonEvent {
                                    id: old,
                                    action: ButtonAction::Short(self.tap_count),
                                });
                            self.tap_count = 1;
                            self.last_release = Instant::now();
                            if let Some(event) = pending {
                                return event;
                            }
                        }
                    }
                }
            }
        }
    }
}
