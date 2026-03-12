// SPDX-License-Identifier: MIT OR Apache-2.0
//! BLE radio driver wrapper around `esp-radio`.
//!
//! Initializes the ESP32/ESP32-S3 radio coprocessor and returns a
//! `BleConnector` for use with the `trouble-host` BLE stack.
//!
//! Ref: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/bluetooth/index.html>
use esp_hal::peripherals::BT;
use esp_radio::{
    InitializationError,
    ble::{Config, InvalidConfigError, controller::BleConnector},
};
use static_cell::make_static;
use thiserror_no_std::Error;

#[derive(Debug, Error)]
pub enum RadioError {
    #[error("Failed to initialize WIFI/BLE controller")]
    WifiInitError(#[from] InitializationError),

    #[error("Failed to initialize WIFI/BLE controller")]
    BleConfigError(#[from] InvalidConfigError),
}

pub struct WifiDriver {
    pub ble_connector: BleConnector<'static>,
}

impl WifiDriver {
    pub fn new(bt: BT<'static>) -> Result<Self, RadioError> {
        let radio = make_static!(esp_radio::init()?);
        let ble_connector = BleConnector::new(radio, bt, Config::default().with_task_priority(10))?;

        Ok(Self { ble_connector })
    }
}
