// SPDX-License-Identifier: MIT OR Apache-2.0
//! Radio bring-up for the ESP32/ESP32-S3 coprocessor.
//!
//! The on-package radio is shared between **BLE** and **WiFi** — both are
//! sub-functions of the same hardware. Each lives in its own submodule and is
//! gated by a cargo feature, so a binary only compiles in (and pays the RAM cost
//! of) the radios it actually uses:
//!
//! | Feature    | Module    | Provides                                  |
//! |------------|-----------|-------------------------------------------|
//! | `ble`      | [`ble`]   | `BleConnector` for the `trouble-host` stack |
//! | `wifi`     | [`wifi`]  | WiFi controller + scanning                |
//! | `wifi-sta` | [`wifi`]  | STA + DHCP `embassy_net::Stack` (adds `embassy-net`) |
//! | `coex`     | —         | software WiFi/BLE coexistence             |
//!
//! Coexistence (`coex`) lets BLE and WiFi run at the same time at the cost of
//! extra RAM (~96 KB reclaimed on ESP32, less on ESP32-S3). It is only compiled
//! in when an application enables the `coex` feature.
//!
//! In all cases `esp_rtos::start(..)` must run before any radio is created — the
//! radio drivers schedule their work on the esp-rtos executor.

#[cfg(feature = "ble")]
pub mod ble;

#[cfg(feature = "wifi")]
pub mod wifi;
