// SPDX-License-Identifier: BSD-3-Clause
use esp_hal::{gpio::AnyPin, peripherals::RMT, rmt::Rmt, time::Rate};
use esp_hal_rmt_onewire::{Address, OneWire, Search};
use heapless::index_map::FnvIndexMap;
use log::{debug, trace};
use thiserror_no_std::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to init RMT")]
    RmtDriverError(#[from] esp_hal::rmt::Error),
    #[error("OneWire bus is not in idle state - pull-ups present?")]
    HardwareError,
    #[error("Failed to read temperature")]
    ReadTemperatureError,
    #[error("Other OneWire error")]
    OneWireError(#[from] esp_hal_rmt_onewire::Error),
    #[error("Number of connected sensors larger than expected")]
    TooManySensorsConnected,
}

pub struct Ds16b20Driver {
    ow: OneWire<'static>,
}

impl Ds16b20Driver {
    const MAX_SENSORS: usize = 16;
    pub fn new(rmt: RMT<'static>, pin: AnyPin<'static>) -> Result<Self, Error> {
        let rmt = Rmt::new(rmt, Rate::from_mhz(80_u32)).map_err(|_| Error::HardwareError)?.into_async();
        // ESP32: TX channel 0, RX channel 2
        // ESP32-S3: TX channels 0-3, RX channels 4-7
        #[cfg(feature = "fire27")]
        let ow = OneWire::new(rmt.channel0, rmt.channel2, pin)?;
        #[cfg(feature = "cores3")]
        let ow = OneWire::new(rmt.channel0, rmt.channel4, pin)?;
        Ok(Ds16b20Driver { ow })
    }

    pub async fn read_all_temperatures(&mut self) -> Result<impl Iterator<Item = (Address, f32)>, Error> {
        trace!("Resetting the bus");
        self.ow.reset().await?;

        trace!("Broadcasting a measure temperature command to all attached sensors");
        for a in [0xCC, 0x44] {
            self.ow.send_byte(a).await?;
        }

        trace!("Scanning the bus to retrieve the measured temperatures");
        let mut search = Search::new();
        let mut temperatures = FnvIndexMap::<_, _, { Self::MAX_SENSORS }>::new();
        loop {
            match search.next(&mut self.ow).await {
                Ok(address) => {
                    debug!("Reading device {:?}", address);
                    self.ow.reset().await?;
                    self.ow.send_byte(0x55).await?;
                    self.ow.send_address(address).await?;
                    self.ow.send_byte(0xBE).await?;
                    let temp_low = self.ow.exchange_byte(0xFF).await.map_err(|_| Error::ReadTemperatureError)?;
                    let temp_high = self.ow.exchange_byte(0xFF).await.map_err(|_| Error::ReadTemperatureError)?;
                    let temperature_celsius: f32 = fixed::types::I12F4::from_le_bytes([temp_low, temp_high]).into();
                    temperatures.insert(address, temperature_celsius).map_err(|_| Error::TooManySensorsConnected)?;
                    debug!("sensor 0x{:x}: {}°C", address, temperature_celsius);
                }
                Err(_) => {
                    trace!("End of search");
                    break;
                }
            }
        }
        Ok(temperatures.into_iter())
    }
}
