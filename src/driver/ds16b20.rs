// SPDX-License-Identifier: MIT OR Apache-2.0
//! DS18B20 1-Wire digital temperature sensor driver via RMT peripheral.
//!
//! Broadcasts a single Convert T to all sensors, waits for the conversion to
//! complete, then uses the ROM Search algorithm to enumerate every sensor and
//! reads each device's full 9-byte scratchpad (validating its CRC-8). The
//! temperature (bytes 0..1) is returned as signed 12.4 fixed-point (0.0625 °C
//! resolution).
//!
//! 1-Wire commands used:
//!   0xCC  Skip ROM — address all devices
//!   0x44  Convert T — start temperature conversion (750 ms max at 12-bit)
//!   0x55  Match ROM — address specific device
//!   0xBE  Read Scratchpad — returns 9 bytes (TEMP_LSB, TEMP_MSB, …, CRC)
//!
//! RMT channel assignment (chip-specific):
//!   ESP32 (fire27):   TX=channel0, RX=channel2
//!   ESP32-S3 (cores3): TX=channel0, RX=channel4
//!
//! Datasheet: <https://www.analog.com/media/en/technical-documentation/data-sheets/DS18B20.pdf>
use crate::driver::onewire::{Address, OneWire, Search, SearchError, crc8};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::{gpio::AnyPin, peripherals::RMT, rmt::Rmt, time::Rate};
use heapless::index_map::FnvIndexMap;
use thiserror_no_std::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to init RMT")]
    RmtDriverError(#[from] esp_hal::rmt::Error),
    #[error("OneWire bus is not in idle state - pull-ups present?")]
    HardwareError,
    #[error("Failed to read temperature")]
    ReadTemperatureError,
    #[error("Temperature conversion did not complete within 750 ms")]
    ConversionTimeout,
    #[error("Other OneWire error")]
    OneWireError(#[from] crate::driver::onewire::Error),
    #[error("Number of connected sensors larger than expected")]
    TooManySensorsConnected,
}

pub struct Ds16b20Driver {
    ow: OneWire<'static>,
}

impl Ds16b20Driver {
    const MAX_SENSORS: usize = 16;
    pub fn new(rmt: RMT<'static>, pin: AnyPin<'static>) -> Result<Self, Error> {
        let rmt = Rmt::new(rmt, Rate::from_mhz(80_u32))
            .map_err(|_| Error::HardwareError)?
            .into_async();
        // ESP32: TX channel 0, RX channel 2
        // ESP32-S3: TX channels 0-3, RX channels 4-7
        #[cfg(feature = "fire27")]
        let ow = OneWire::new(rmt.channel0, rmt.channel2, pin)?;
        #[cfg(feature = "cores3")]
        let ow = OneWire::new(rmt.channel0, rmt.channel4, pin)?;
        Ok(Ds16b20Driver { ow })
    }

    pub async fn read_all_temperatures(
        &mut self,
    ) -> Result<impl Iterator<Item = (Address, f32)>, Error> {
        trace!("Resetting the bus");
        self.ow.reset().await?;

        trace!("Broadcasting a measure temperature command to all attached sensors");
        for a in [0xCC, 0x44] {
            self.ow.send_byte(a).await?;
        }

        // A read immediately after Convert T returns the *previous* conversion
        // (the very first read after power-up is the 85 °C reset value). Wait
        // for completion before scanning so callers get the fresh measurement.
        self.wait_conversion_complete().await?;

        trace!("Scanning the bus to retrieve the measured temperatures");
        let mut search = Search::new();
        let mut temperatures = FnvIndexMap::<_, _, { Self::MAX_SENSORS }>::new();
        loop {
            match search.next(&mut self.ow).await {
                Ok(address) => {
                    debug!("Reading device {:?}", address);
                    match self.read_scratchpad(address).await {
                        Ok(temperature_celsius) => {
                            temperatures
                                .insert(address, temperature_celsius)
                                .map_err(|_| Error::TooManySensorsConnected)?;
                            debug!("sensor {}: {}°C", address, temperature_celsius);
                        }
                        // A single sensor failing its scratchpad CRC must not
                        // abort the whole scan — warn and omit it.
                        Err(e) => warn!("skipping sensor {}: {:?}", address, e),
                    }
                }
                // A corrupt ROM CRC affects only this one enumeration step; the
                // search state has advanced, so keep scanning the rest.
                Err(SearchError::CrcMismatch) => warn!("ROM CRC mismatch, skipping device"),
                Err(SearchError::SearchComplete) | Err(SearchError::NoDevicesPresent) => {
                    trace!("End of search");
                    break;
                }
                Err(SearchError::BusError(e)) => return Err(e.into()),
            }
        }
        Ok(temperatures.into_iter())
    }

    /// Poll the bus until the in-progress temperature conversion completes.
    ///
    /// An externally-powered DS18B20 holds the bus low while converting and
    /// releases it (a read slot returns 1) when finished. Returns
    /// [`Error::ConversionTimeout`] if the 12-bit worst case (750 ms, plus
    /// margin) elapses without completion.
    async fn wait_conversion_complete(&mut self) -> Result<(), Error> {
        let start = Instant::now();
        loop {
            let mut bit = [false; 1];
            self.ow.exchange_bits(&[true], &mut bit).await?;
            if bit[0] {
                return Ok(());
            }
            if start.elapsed() > Duration::from_millis(800) {
                return Err(Error::ConversionTimeout);
            }
            // Conversion is signalled by the read-slot result above, not by this
            // delay; it only paces the poll so we don't hammer the bus.
            Timer::after(Duration::from_millis(10)).await;
        }
    }

    /// Address a single device, read its full 9-byte scratchpad, validate the
    /// CRC, and return the temperature in °C.
    async fn read_scratchpad(&mut self, address: Address) -> Result<f32, Error> {
        self.ow.reset().await?;
        self.ow.send_byte(0x55).await?; // Match ROM
        self.ow.send_address(address).await?;
        self.ow.send_byte(0xBE).await?; // Read Scratchpad
        let mut scratch = [0u8; 9];
        for byte in scratch.iter_mut() {
            *byte = self
                .ow
                .exchange_byte(0xFF)
                .await
                .map_err(|_| Error::ReadTemperatureError)?;
        }
        // Byte 8 is a CRC-8 over bytes 0..8; reject a corrupt read.
        if crc8(&scratch[..8]) != scratch[8] {
            return Err(Error::ReadTemperatureError);
        }
        Ok(fixed::types::I12F4::from_le_bytes([scratch[0], scratch[1]]).into())
    }
}
