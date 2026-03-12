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

/// Convenience loop: ticker + read_rpm + callback.
pub async fn rpm_loop(resources: RpmResources<'static>, config: RpmConfig, on_rpm: fn(f32)) {
    let mut pcnt_driver = resources.into_driver();
    let mut ticker = Ticker::every(Duration::from_millis(config.loop_time_ms));
    loop {
        let rpm = read_rpm(&mut pcnt_driver, &config);
        on_rpm(rpm);
        ticker.next().await;
    }
}
