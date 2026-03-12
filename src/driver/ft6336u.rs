// SPDX-License-Identifier: MIT OR Apache-2.0
//! FT6336U capacitive touch controller driver (I2C 0x38).
//!
//! Reads only touch point 1 (single-finger). Returns screen coordinates
//! or None if no touch is active.
//!
//! Register map (FT6336U datasheet V1.0, §4 "Register Description"):
//!   0x02  TD_STATUS   [3:0] = number of touch points (0–2)
//!   0x03  P1_XH       [7:6] = event flag (00=down, 01=up, 10=contact, 11=reserved)
//!                     [3:0] = X position high nibble
//!   0x04  P1_XL       [7:0] = X position low byte
//!   0x05  P1_YH       [7:6] = (reserved)  [3:0] = Y position high nibble
//!   0x06  P1_YL       [7:0] = Y position low byte
//!
//! Ref: <https://m5stack.oss-cn-shenzhen.aliyuncs.com/resource/docs/datasheet/core/D-FT6336U-DataSheet-V1.020af.pdf>
//! Also: FT6x06 Application Note (Adafruit) for FT6x36 family register compatibility.
use crate::io::shared_i2c::SharedI2cBus;

pub const ADDR: u8 = 0x38;

const REG_TD_STATUS: u8 = 0x02; // burst-read 0x02..0x06 (5 bytes)

/// Read first touch point. Returns `Some((x, y))` if touched, `None` otherwise.
pub async fn read_touch(
    i2c: &SharedI2cBus,
) -> Result<Option<(u16, u16)>, esp_hal::i2c::master::Error> {
    let mut buf = [0u8; 5]; // TD_STATUS, P1_XH, P1_XL, P1_YH, P1_YL
    i2c.lock()
        .await
        .write_read_async(ADDR, &[REG_TD_STATUS], &mut buf)
        .await?;

    let touch_count = buf[0] & 0x0F; // TD_STATUS[3:0]
    if touch_count == 0 {
        return Ok(None);
    }

    let x = ((buf[1] & 0x0F) as u16) << 8 | buf[2] as u16; // P1_XH[3:0] : P1_XL
    let y = ((buf[3] & 0x0F) as u16) << 8 | buf[4] as u16; // P1_YH[3:0] : P1_YL
    Ok(Some((x, y)))
}
