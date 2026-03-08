// SPDX-License-Identifier: BSD-3-Clause
//! PCNT (Pulse Counter) driver for RPM sensing.
//!
//! Uses hardware unit 1 in edge-counting mode: both rising and falling edges
//! increment the counter. The caller reads and resets the count periodically
//! to derive RPM from pulse frequency.
//!
//! ESP32 PCNT reference: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/peripherals/pcnt.html>
use esp_hal::{
    gpio::Input,
    pcnt::{Pcnt, channel, unit},
    peripherals::PCNT,
};

pub struct PcntDriver {
    pub pcnt_unit: unit::Unit<'static, 1>,
}

impl PcntDriver {
    pub fn get_and_reset(&mut self) -> i16 {
        let c = self.pcnt_unit.counter.get();
        self.pcnt_unit.clear();
        c
    }
}

impl PcntDriver {
    pub fn new(pcnt: PCNT<'static>, rpm_pin: Input<'static>) -> Self {
        // Initialize Pulse Counter (PCNT) unit with limits and filter settings
        let pcnt = Pcnt::new(pcnt);
        let u0 = pcnt.unit1;
        u0.clear();

        // Set up channels with control and edge signals
        let ch0 = &u0.channel0;
        ch0.set_edge_signal(rpm_pin);
        ch0.set_input_mode(channel::EdgeMode::Increment, channel::EdgeMode::Increment);

        // Enable interrupts and resume pulse counter unit
        u0.listen();
        u0.resume();
        Self { pcnt_unit: u0 }
    }
}
