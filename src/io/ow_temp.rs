// SPDX-License-Identifier: MIT OR Apache-2.0
use embassy_time::{Duration, Ticker};
use esp_hal::{gpio::AnyPin, peripherals::RMT};

use crate::driver::ds16b20::{Ds16b20Driver, Error};

pub struct OnewireResources<'a> {
    pub rmt: RMT<'a>,
    pub pin: AnyPin<'a>,
}

impl OnewireResources<'static> {
    pub fn into_driver(self) -> Result<Ds16b20Driver, Error> {
        Ok(Ds16b20Driver::new(self.rmt, self.pin)?)
    }
}

const TEMP_LOOP_TIME_MS: u64 = 3000;

/// 3s ticker, reads all sensors, calls on_temperatures with slice of (address, temp).
pub async fn ow_loop(resources: OnewireResources<'static>, on_temperatures: fn(&[(u64, f32)])) {
    let Ok(mut ow) = resources.into_driver() else {
        error!("failed to init OneWire bus - pull-up resistor present?");
        return;
    };

    let mut ticker = Ticker::every(Duration::from_millis(TEMP_LOOP_TIME_MS));
    loop {
        debug!("reading temperatures");
        let Ok(temperatures) = ow.read_all_temperatures().await else {
            error!("Error while accessing OneWire bus, terminating task");
            return;
        };

        let mut buf: heapless::Vec<(u64, f32), 2> = heapless::Vec::new();
        for (addr, temp) in temperatures {
            debug!("temperature of {}C from sensor 0x{:x}", temp, addr.0);
            if buf.push((addr.0, temp)).is_err() {
                warn!("more than 2 sensors, ignoring extras");
                break;
            }
        }
        on_temperatures(buf.as_slice());
        ticker.next().await;
    }
}
