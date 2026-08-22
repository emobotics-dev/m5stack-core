// SPDX-License-Identifier: MIT OR Apache-2.0
use embassy_time::{Duration, Ticker};
use esp_hal::gpio::{AnyPin, Input, InputConfig};

use crate::driver::pcnt::PcntDriver;

/// Sampling window for the pulse counter.
///
/// Deliberately the ONLY knob here. Pole pairs and pulley geometry describe the
/// alternator and the engine it is bolted to, not the board — a BSP that knows
/// them cannot be reused on a different vehicle. The application converts the
/// pulse rate below into engine rpm.
pub struct RpmConfig {
    pub loop_time_ms: u64,
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

/// Single-shot read of the sensor's pulse rate, in Hz.
///
/// What the hardware actually measures. Turning Hz into engine rpm needs the
/// alternator's pole count and the pulley ratio, which are the application's
/// to know.
pub fn read_pulse_hz(pcnt: &mut PcntDriver, config: &RpmConfig) -> f32 {
    let pulse_count = pcnt.get_and_reset();
    pulse_count as f32 * (1000. / config.loop_time_ms as f32) // intervals per second
}

/// Number of consecutive zero-pulse intervals before we report "no signal"
/// (NaN) instead of "engine stopped" (0.0). At the default 100 ms interval
/// this is 3 s.
const NO_SIGNAL_INTERVALS: u32 = 30;

/// Convenience loop: ticker + [`read_pulse_hz`] + callback.
///
/// `on_pulse_hz` is invoked only on **state transitions**, so callers that
/// store into a shared atomic don't get hammered every 100 ms:
///   * each non-zero reading       → pulse rate in Hz
///   * first zero after non-zero   → `0.0` (engine just stopped)
///   * [`NO_SIGNAL_INTERVALS`] zeros → `f32::NAN` (signal lost)
///
/// Both sentinels survive any linear Hz→rpm conversion the caller applies
/// (`0.0 * k == 0.0`, `NAN * k == NAN`), so the application does not have to
/// special-case them.
///
/// Between the "stopped" and "no signal" transitions the callback is
/// silent, which lets HIL-injected values stick once the rig has been
/// idle long enough.
pub async fn pulse_loop(resources: RpmResources<'static>, config: RpmConfig, on_pulse_hz: fn(f32)) {
    let mut pcnt_driver = resources.into_driver();
    let mut ticker = Ticker::every(Duration::from_millis(config.loop_time_ms));
    let mut zero_intervals: u32 = 0;
    loop {
        let hz = read_pulse_hz(&mut pcnt_driver, &config);
        if hz > 0.0 {
            zero_intervals = 0;
            on_pulse_hz(hz);
        } else {
            let prev = zero_intervals;
            zero_intervals = zero_intervals.saturating_add(1);
            if prev == 0 {
                on_pulse_hz(0.0); // transition: running → stopped
            } else if prev == NO_SIGNAL_INTERVALS {
                on_pulse_hz(f32::NAN); // transition: stopped → no signal
            }
            // else: PROCESS_DATA untouched
        }
        ticker.next().await;
    }
}
