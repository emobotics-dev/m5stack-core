// SPDX-License-Identifier: MIT OR Apache-2.0
pub mod buttons;
pub mod console;
pub mod ow_temp;
pub mod pps;
pub mod rpm;
pub mod shared_i2c;
#[cfg(feature = "serial-cmd")]
pub mod serial_cmd;
pub mod touch_buttons;
pub mod watchdog;
