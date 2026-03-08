// SPDX-License-Identifier: BSD-3-Clause
//! AW9523B I2C GPIO expander driver for M5Stack CoreS3 (I2C 0x58).
//!
//! Register map (AW9523B English datasheet V1.5, §5 "Register Description"):
//!   0x02  Output P0   output latch for port 0 (default 0x00)
//!   0x03  Output P1   output latch for port 1 (default 0x00)
//!   0x04  Config P0   direction: 0=output, 1=input (default 0xFF = all input)
//!   0x05  Config P1   direction: 0=output, 1=input (default 0xFF = all input)
//!   0x12  LEDMODE P0  0=LED current mode, 1=GPIO push-pull (default 0x00)
//!   0x13  LEDMODE P1  0=LED current mode, 1=GPIO push-pull (default 0x00)
//!
//! Pin assignment (M5Stack CoreS3 v1.1 schematic, page 4):
//!   P0_0  TOUCH_RST   active-LOW reset for FT6336U
//!   P0_1  BUS_OUT_EN  active-LOW — enables 5V boost (DANGER if driven LOW on USB power)
//!   P0_2  AW_RST      self-reset (active-LOW)
//!   P0_5  USB_OTG_EN  active-HIGH — enables USB OTG (kills USB-JTAG if asserted)
//!   P1_0  CAM_RST     camera reset (active-LOW)
//!   P1_1  LCD_RST     ILI9342C reset (active-LOW)
//!   P1_2  TOUCH_INT   FT6336U interrupt (input)
//!   P1_3  AW_INT      AW9523B interrupt output (active-LOW, open-drain)
//!
//! **SAFETY**: P0 registers are modified via read-modify-write, touching ONLY
//! the P0_0 (TOUCH_RST) bit. All other P0 bits are preserved at power-on
//! defaults to avoid activating BUS_OUT_EN or USB_OTG_EN.
//!
//! Datasheet: <https://m5stack.oss-cn-shenzhen.aliyuncs.com/resource/docs/products/core/CoreS3/AW9523B-EN.pdf>
//! Also: <https://cdn-shop.adafruit.com/product-files/4886/AW9523+English+Datasheet.pdf>
use embassy_time::{Duration, Timer};
use thiserror_no_std::Error;

use crate::io::shared_i2c::SharedI2cBus;

pub const ADDR: u8 = 0x58;

const REG_OUTPUT_P0: u8 = 0x02;
const REG_OUTPUT_P1: u8 = 0x03;
const REG_CONFIG_P0: u8 = 0x04;
const REG_CONFIG_P1: u8 = 0x05;
const REG_LEDMODE_P0: u8 = 0x12;
const REG_LEDMODE_P1: u8 = 0x13;

// P0 pin masks
pub const P0_TOUCH_RST: u8 = 0x01; // P0_0
#[allow(dead_code)] pub const P0_AW_RST: u8 = 0x04; // P0_2

// P1 pin masks
pub const P1_CAM_RST: u8 = 0x01; // P1_0 (output)
pub const P1_LCD_RST: u8 = 0x02; // P1_1 (output)
#[allow(dead_code)] pub const P1_TOUCH_INT: u8 = 0x04; // P1_2 (input)
#[allow(dead_code)] pub const P1_AW_INT: u8 = 0x08; // P1_3 (input)

// P1: P1_0 (CAM_RST), P1_1 (LCD_RST) → outputs; rest inputs
const CONFIG_P1_VAL: u8 = !(P1_CAM_RST | P1_LCD_RST);

#[derive(Debug, Error)]
pub enum Aw9523bError {
    #[error("I2C error: {0:?}")]
    I2cError(#[from] esp_hal::i2c::master::Error),
}

pub struct Aw9523bDriver {
    i2c: &'static SharedI2cBus,
    address: u8,
}

pub struct Aw9523bResources {
    pub i2c: &'static SharedI2cBus,
}

impl Aw9523bDriver {
    pub fn new(res: Aw9523bResources) -> Self {
        Self { i2c: res.i2c, address: ADDR }
    }

    async fn read_reg(&mut self, reg: u8) -> Result<u8, Aw9523bError> {
        let mut buf = [0u8];
        self.i2c.lock().await.write_read_async(self.address, &[reg], &mut buf).await?;
        Ok(buf[0])
    }

    async fn write_reg(&mut self, reg: u8, value: u8) -> Result<(), Aw9523bError> {
        self.i2c.lock().await.write_async(self.address, &[reg, value]).await?;
        Ok(())
    }

    /// Read-modify-write: set bit(s) in register.
    async fn set_bits(&mut self, reg: u8, mask: u8) -> Result<(), Aw9523bError> {
        let val = self.read_reg(reg).await?;
        self.write_reg(reg, val | mask).await
    }

    /// Read-modify-write: clear bit(s) in register.
    async fn clear_bits(&mut self, reg: u8, mask: u8) -> Result<(), Aw9523bError> {
        let val = self.read_reg(reg).await?;
        self.write_reg(reg, val & !mask).await
    }

    /// Set LEDMODE to GPIO, configure P1 outputs, set up P0_0 (TOUCH_RST) via RMW.
    pub async fn init(&mut self) -> Result<(), Aw9523bError> {
        // P0: RMW — set TOUCH_RST latch HIGH before making it an output
        self.set_bits(REG_OUTPUT_P0, P0_TOUCH_RST).await?;
        self.set_bits(REG_LEDMODE_P0, P0_TOUCH_RST).await?; // GPIO mode for P0_0 only
        self.clear_bits(REG_CONFIG_P0, P0_TOUCH_RST).await?; // P0_0 = output (0)
        // P1: full writes (we own all P1 pins)
        self.write_reg(REG_LEDMODE_P1, 0xFF).await?;
        self.write_reg(REG_OUTPUT_P1, P1_CAM_RST | P1_LCD_RST).await?;
        self.write_reg(REG_CONFIG_P1, CONFIG_P1_VAL).await?;
        Ok(())
    }

    /// Pulse LCD_RST (P1_1) low for ≥10 µs, then wait 120 ms for ILI9342C stabilisation.
    pub async fn lcd_rst_pulse(&mut self) -> Result<(), Aw9523bError> {
        self.write_reg(REG_OUTPUT_P1, P1_CAM_RST).await?; // LCD_RST=0, CAM_RST=1
        Timer::after(Duration::from_micros(10)).await;
        self.write_reg(REG_OUTPUT_P1, P1_CAM_RST | P1_LCD_RST).await?;
        Timer::after(Duration::from_millis(120)).await;
        Ok(())
    }

    /// Pulse TOUCH_RST (P0_0) low for 5 ms, then wait 300 ms for FT6336U boot.
    pub async fn touch_rst_pulse(&mut self) -> Result<(), Aw9523bError> {
        self.clear_bits(REG_OUTPUT_P0, P0_TOUCH_RST).await?;
        Timer::after(Duration::from_millis(5)).await;
        self.set_bits(REG_OUTPUT_P0, P0_TOUCH_RST).await?;
        Timer::after(Duration::from_millis(300)).await;
        Ok(())
    }
}
