// SPDX-License-Identifier: MIT OR Apache-2.0
//! Minimal AXP2101 PMIC driver for M5Stack CoreS3 (I2C 0x34).
//!
//! Only DLDO1 (backlight), battery voltage ADC, and VBUS detection are used.
//!
//! Register map (verified against XPowersLib / CircuitPython_AXP2101):
//!   0x00  Power status    bit 3 = VBUS present
//!   0x34  VBAT ADC high   bits[5:0] = high 6 bits of 14-bit reading (1 mV/LSB)
//!   0x35  VBAT ADC low    bits[7:0] = low 8 bits
//!   0x90  LDO enable      bit 7 = DLDO1 enable
//!   0x99  DLDO1 voltage   bits[4:0] = (mV − 500) / 100, range 500–3400 mV
//!
//! Datasheet: <https://m5stack.oss-cn-shenzhen.aliyuncs.com/resource/docs/products/core/CoreS3/AXP2101_Datasheet_V1.1_en.pdf>
//! Also: <https://github.com/lewisxhe/XPowersLib> (register reference)
use thiserror_no_std::Error;

use crate::io::shared_i2c::SharedI2cBus;

const REG_PWR_STATUS: u8 = 0x00;
const REG_LDO_EN: u8 = 0x90;
const REG_DLDO1_VOL: u8 = 0x99;
const REG_VBAT_H: u8 = 0x34;

const DLDO1_VOL_MIN_MV: u16 = 500;
const DLDO1_VOL_STEP_MV: u16 = 100;
const DLDO1_EN_BIT: u8 = 0x80; // bit 7
const VBUS_PRESENT_BIT: u8 = 0x08; // bit 3

#[derive(Debug, Error)]
pub enum Axp2101Error {
    #[error("I2C error: {0:?}")]
    I2cError(#[from] esp_hal::i2c::master::Error),

    #[error("Voltage out of range")]
    VoltageOutOfRange,
}

pub struct Axp2101Driver {
    i2c: &'static SharedI2cBus,
    address: u8,
}

impl Axp2101Driver {
    pub fn new(i2c: &'static SharedI2cBus, address: u8) -> Self {
        Self { i2c, address }
    }

    async fn read_reg(&mut self, reg: u8) -> Result<u8, Axp2101Error> {
        let mut buf = [0u8; 1];
        self.i2c
            .lock()
            .await
            .write_read_async(self.address, &[reg], &mut buf)
            .await?;
        debug!("AXP2101 rd 0x{:02x} = 0x{:02x}", reg, buf[0]);
        Ok(buf[0])
    }

    async fn write_reg(&mut self, reg: u8, value: u8) -> Result<(), Axp2101Error> {
        debug!("AXP2101 wr 0x{:02x} = 0x{:02x}", reg, value);
        self.i2c
            .lock()
            .await
            .write_async(self.address, &[reg, value])
            .await?;
        Ok(())
    }

    /// Enable or disable DLDO1 and set output voltage (mV). Range: 500–3400 mV, 100 mV steps.
    pub async fn set_dldo1(&mut self, enabled: bool, mv: u16) -> Result<(), Axp2101Error> {
        if mv < DLDO1_VOL_MIN_MV || mv > 3400 || (mv - DLDO1_VOL_MIN_MV) % DLDO1_VOL_STEP_MV != 0 {
            return Err(Axp2101Error::VoltageOutOfRange);
        }
        let vol_val = ((mv - DLDO1_VOL_MIN_MV) / DLDO1_VOL_STEP_MV) as u8;
        debug!(
            "AXP2101 DLDO1 enabled={} {}mV (reg_val={})",
            enabled, mv, vol_val
        );
        self.write_reg(REG_DLDO1_VOL, vol_val).await?;

        let en_reg = self.read_reg(REG_LDO_EN).await?;
        let en_val = if enabled {
            en_reg | DLDO1_EN_BIT
        } else {
            en_reg & !DLDO1_EN_BIT
        };
        self.write_reg(REG_LDO_EN, en_val).await?;
        Ok(())
    }

    /// Read battery voltage in mV (14-bit ADC, 1 mV/LSB).
    pub async fn battery_voltage_mv(&mut self) -> Result<u16, Axp2101Error> {
        let hi = self.read_reg(REG_VBAT_H).await?;
        let mut lo_buf = [0u8; 1];
        self.i2c
            .lock()
            .await
            .write_read_async(self.address, &[REG_VBAT_H + 1], &mut lo_buf)
            .await?;
        debug!("AXP2101 rd 0x{:02x} = 0x{:02x}", REG_VBAT_H + 1, lo_buf[0]);
        let raw = ((hi as u16 & 0x3F) << 8) | lo_buf[0] as u16;
        Ok(raw)
    }

    /// Returns true if VBUS (USB power) is present.
    pub async fn vbus_present(&mut self) -> Result<bool, Axp2101Error> {
        let status = self.read_reg(REG_PWR_STATUS).await?;
        Ok(status & VBUS_PRESENT_BIT != 0)
    }
}
