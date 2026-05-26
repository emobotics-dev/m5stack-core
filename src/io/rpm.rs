// SPDX-License-Identifier: MIT OR Apache-2.0
use embassy_time::{Duration, Ticker};
use esp_hal::gpio::{AnyPin, Input, InputConfig};

use crate::driver::pcnt::PcntDriver;

pub struct RpmConfig {
    pub loop_time_ms: u64,
    pub pole_pairs: f32,
    pub pulley_ratio: f32,
}

pub struct RpmResources<'a> {
    pub pcnt: esp_hal::peripherals::PCNT<'a>,
    pub pin: AnyPin<'a>,
}

impl RpmResources<'static> {
    pub fn into_driver(self) -> PcntDriver {
        let input = Input::new(
            self.pin,
            InputConfig::default().with_pull(esp_hal::gpio::Pull::Down),
        );
        PcntDriver::new(self.pcnt, input)
    }
}

/// Single-shot RPM read. Calls pcnt.get_and_reset(), applies config.
pub fn read_rpm(pcnt: &mut PcntDriver, config: &RpmConfig) -> f32 {
    let pulse_count = pcnt.get_and_reset();
    pulse_count as f32
        * 60.                                  // Hz -> rpm
        * (1. / config.pole_pairs / 2.)        // pole pairs, 2 imp per rev
        * (1000. / config.loop_time_ms as f32) // intervals per second
        * config.pulley_ratio
}

/// Number of consecutive zero-pulse intervals before we report "no signal"
/// (NaN) instead of "engine stopped" (0.0). At the default 100 ms interval
/// this is 3 s.
const NO_SIGNAL_INTERVALS: u32 = 30;

/// Convenience loop: ticker + read_rpm + callback.
///
/// `on_rpm` is invoked only on **state transitions**, so callers that
/// store into a shared atomic don't get hammered every 100 ms:
///   * each non-zero reading       → `rpm` (measured value)
///   * first zero after non-zero   → `0.0` (engine just stopped)
///   * [`NO_SIGNAL_INTERVALS`] zeros → `f32::NAN` (signal lost)
///
/// Between the "stopped" and "no signal" transitions the callback is
/// silent, which lets HIL-injected values stick once the rig has been
/// idle long enough.
pub async fn rpm_loop(resources: RpmResources<'static>, config: RpmConfig, on_rpm: fn(f32)) {
    let mut pcnt_driver = resources.into_driver();
    let mut ticker = Ticker::every(Duration::from_millis(config.loop_time_ms));
    let mut zero_intervals: u32 = 0;
    loop {
        let rpm = read_rpm(&mut pcnt_driver, &config);
        if rpm > 0.0 {
            zero_intervals = 0;
            on_rpm(rpm);
        } else {
            let prev = zero_intervals;
            zero_intervals = zero_intervals.saturating_add(1);
            if prev == 0 {
                on_rpm(0.0);                // transition: running → stopped
            } else if prev == NO_SIGNAL_INTERVALS {
                on_rpm(f32::NAN);            // transition: stopped → no signal
            }
            // else: PROCESS_DATA untouched
        }
        ticker.next().await;
    }
}
