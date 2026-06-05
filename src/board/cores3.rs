// SPDX-License-Identifier: MIT OR Apache-2.0
//! M5Stack CoreS3 board-level bring-up sequences.

use crate::driver::aw9523b::{Aw9523bDriver, Aw9523bResources};
use crate::driver::axp2101::Axp2101Driver;
use crate::io::shared_i2c::SharedI2cBus;

/// AXP2101 PMIC I2C address on CoreS3.
const AXP2101_ADDR: u8 = 0x34;
/// Display backlight rail: AXP2101 DLDO1 @ 3.3 V.
const BACKLIGHT_MV: u16 = 3300;

/// Power + display bring-up over the shared I2C bus: AW9523B IO-expander
/// (`LCD_RST` + touch reset) then AXP2101 PMIC backlight (DLDO1 @ 3.3 V).
///
/// Best-effort — each sub-step logs and continues on error, so a flaky chip
/// can't block the display/control loop from coming up. Caller-driven so the
/// caller owns the *when* and the *which core*: this MUST run on the core that
/// owns the I2C IRQ (APP core on the regulator — see its `irq-core-binding`
/// doc), and the caller signals display-readiness after it returns
/// (`LCD_RST` must precede display init).
pub async fn power_display_reset(i2c: &'static SharedI2cBus) {
    let mut aw = Aw9523bDriver::new(Aw9523bResources { i2c });
    if let Err(e) = aw.init().await {
        error!("AW9523B init failed: {:?}", e);
    }
    if let Err(e) = aw.lcd_rst_pulse().await {
        error!("AW9523B LCD RST failed: {:?}", e);
    }
    if let Err(e) = aw.touch_rst_pulse().await {
        error!("AW9523B TOUCH RST failed: {:?}", e);
    }
    let mut axp = Axp2101Driver::new(i2c, AXP2101_ADDR);
    if let Err(e) = axp.set_dldo1(true, BACKLIGHT_MV).await {
        error!("AXP2101 backlight enable failed: {:?}", e);
    }
}
