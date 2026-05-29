// SPDX-License-Identifier: MIT OR Apache-2.0
//! BLE radio driver wrapper around `esp-radio`.
//!
//! Initializes the ESP32/ESP32-S3 radio coprocessor and returns a
//! `BleConnector` for use with the `trouble-host` BLE stack.
//!
//! Ref: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/bluetooth/index.html>
use esp_hal::peripherals::BT;
use esp_radio::ble::{Config, InvalidConfigError, controller::{BleConnector, BleInitError}};
use thiserror_no_std::Error;

#[derive(Debug, Error)]
pub enum RadioError {
    #[error("Failed to initialize BLE controller")]
    BleInitError(#[from] BleInitError),

    #[error("Failed to initialize BLE controller")]
    BleConfigError(#[from] InvalidConfigError),
}

pub struct WifiDriver {
    pub ble_connector: BleConnector<'static>,
}

impl WifiDriver {
    /// Initialize BLE in scan-only mode with minimal buffer footprint.
    ///
    /// esp-radio's `Config::default()` sizes the BT-controller blob for
    /// `max_connections=6` and `scan_duplicate_list_count=100`. On ESP32-S3
    /// that defaults to ~36 KB of heap, which doesn't fit alongside LVGL on
    /// cores3 (56 KB heap). Our use case is passive scanning of Victron
    /// advertisements — we never open a connection, and only a handful of
    /// distinct advertisers exist on the bus.
    ///
    /// `max_connections=1` is the minimum the controller accepts. Setting
    /// it to 1 shrinks the per-connection static state. Combined with a
    /// 10-entry duplicate-filter (the spec minimum, range 10–1000), this
    /// frees ~10 KB on ESP32-S3 and a smaller amount on ESP32.
    pub fn new(bt: BT<'static>) -> Result<Self, RadioError> {
        // task_stack_size: BLE controller's own task stack (esp-radio default
        // is 4096 B). The HIL hit a stack-guard watchpoint in
        // `ld_sscan_frm_cbk` (controller's scan-frame callback): SP within
        // 32 B of the per-task guard while a `r_rwbtdm_isr_wrapper` was
        // nesting on top. Bumping to 8 KB gives the controller's deepest
        // ISR-nested path room without resizing the main task stack region
        // (which is pinned at [28, 80] KB by ESP_HAL_CONFIG_MAIN_STACK_*).
        // Costs 4 KB of heap — well within budget on both targets.
        let cfg = Config::default()
            .with_task_priority(10)
            .with_max_connections(1)
            .with_scan_duplicate_list_count(10)
            .with_task_stack_size(8192);
        let ble_connector = BleConnector::new(bt, cfg)?;

        Ok(Self { ble_connector })
    }
}
