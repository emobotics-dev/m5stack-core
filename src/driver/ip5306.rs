// SPDX-License-Identifier: MIT OR Apache-2.0
//! IP5306 power-management / battery-gauge driver (Fire27 & classic Core).
//!
//! The **I2C-enabled IP5306** at address `0x75` is the battery charger/gauge on
//! the PMIC-less classic ESP32 cores: the M5Stack **Fire** carries one onboard,
//! and the **M5GO Battery Bottom (A014)** carries one to give the Basic Core
//! battery management. It sits on the M-Bus internal I2C bus (`SharedI2cBus`),
//! which on the Fire is `G21/G22`. The chip exposes only a coarse 4-step fuel
//! gauge plus charge / charge-done flags — no per-mV ADC.
//!
//! **Not used on CoreS3.** The CoreS3 has its own PMIC (AXP2101 @ `0x34`) that
//! manages the battery — including the M5GO bottom's cell on the BAT pin — so
//! read the battery there via [`crate::driver::axp2101`], not here. An A014
//! bottom's IP5306 does not appear on the CoreS3 I2C scan.
//!
//! Register map (matches the M5Stack `Power`/UIFlow IP5306 usage):
//!   0x70  READ0   bit 3 = charging in progress (CHARGE_ENABLE)
//!   0x71  READ1   bit 3 = charge complete (battery full)
//!   0x78  READ4   bits[7:4] = remaining-gauge code (see [`battery_level`])
//!
//! The `0x78` high nibble is the chip's LED-gauge encoding, **not** a linear
//! percentage — it counts how many of the four gauge LEDs are *unlit*, so the
//! value runs "backwards". [`Ip5306Driver::battery_level`] reproduces the exact
//! mapping M5Stack uses (`0xF0→0%, 0xE0→25%, 0xC0→50%, 0x80→75%, 0x00→100%`).
//!
//! Refs: <https://docs.m5stack.com/en/core/fire> (IP5306 @ 0x75),
//! <https://zenn.dev/tomorrow56/articles/43f64daa279510?locale=en> (register map)
use thiserror_no_std::Error;

use crate::io::shared_i2c::SharedI2cBus;

/// IP5306 I2C address on the M-Bus (7-bit; the chip's raw 8-bit slave addr is 0xEA).
pub const IP5306_ADDR: u8 = 0x75;

const REG_READ0: u8 = 0x70; // charge status
const REG_READ1: u8 = 0x71; // charge-full status
const REG_READ4: u8 = 0x78; // battery gauge

const CHARGE_BIT: u8 = 1 << 3; // READ0 bit 3
const CHARGE_FULL_BIT: u8 = 1 << 3; // READ1 bit 3
const GAUGE_MASK: u8 = 0xF0; // READ4 high nibble

#[derive(Debug, Error)]
pub enum Ip5306Error {
    #[error("I2C error: {0:?}")]
    I2cError(#[from] esp_hal::i2c::master::Error),
}

pub struct Ip5306Driver {
    i2c: &'static SharedI2cBus,
    address: u8,
}

impl Ip5306Driver {
    /// Construct a driver on the shared bus. Use [`IP5306_ADDR`] for the address.
    pub fn new(i2c: &'static SharedI2cBus, address: u8) -> Self {
        Self { i2c, address }
    }

    async fn read_reg(&mut self, reg: u8) -> Result<u8, Ip5306Error> {
        let mut buf = [0u8; 1];
        self.i2c
            .lock()
            .await
            .write_read_async(self.address, &[reg], &mut buf)
            .await?;
        debug!("IP5306 rd 0x{:02x} = 0x{:02x}", reg, buf[0]);
        Ok(buf[0])
    }

    /// Probe whether an IP5306 actually answers at this address (the bottom may
    /// not be attached). Returns `Ok(true)` if register 0x70 reads back.
    pub async fn present(&mut self) -> bool {
        self.read_reg(REG_READ0).await.is_ok()
    }

    /// Coarse battery level in percent: one of `0, 25, 50, 75, 100`.
    ///
    /// The IP5306 only reports a 4-LED gauge; the high nibble of register 0x78
    /// is its "LEDs remaining off" code, mapped here exactly as M5Stack does.
    /// Any unexpected nibble (e.g. `0xF0`) reads as `0`.
    pub async fn battery_level(&mut self) -> Result<u8, Ip5306Error> {
        let gauge = self.read_reg(REG_READ4).await? & GAUGE_MASK;
        Ok(match gauge {
            0xE0 => 25,
            0xC0 => 50,
            0x80 => 75,
            0x00 => 100,
            _ => 0, // 0xF0 and any other pattern
        })
    }

    /// True while the battery is charging (USB/charge-base power present).
    pub async fn is_charging(&mut self) -> Result<bool, Ip5306Error> {
        Ok(self.read_reg(REG_READ0).await? & CHARGE_BIT != 0)
    }

    /// True once charging has completed (battery full).
    pub async fn is_charge_full(&mut self) -> Result<bool, Ip5306Error> {
        Ok(self.read_reg(REG_READ1).await? & CHARGE_FULL_BIT != 0)
    }
}
