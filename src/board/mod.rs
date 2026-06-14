// SPDX-License-Identifier: MIT OR Apache-2.0
//! Board-level bring-up: pin wiring ([`cores3::Board`]/[`fire27::Board`]),
//! display + SD shared-bus sequences ([`spi2`], [`display`]), and bring-up
//! orchestration across drivers.

#[cfg(feature = "cores3")]
pub mod cores3;
#[cfg(feature = "display")]
pub mod display;
#[cfg(feature = "fire27")]
pub mod fire27;
#[cfg(any(feature = "cores3", feature = "fire27"))]
pub mod spi2;

use esp_hal::{
    interrupt::software::SoftwareInterruptControl,
    peripherals::{CPU_CTRL, LPWR, Peripherals},
    timer::AnyTimer,
};

/// On-board display resolution — a *board fact*, independent of the `display`
/// driver feature, defined exactly once per target here. Every subsystem that
/// needs it (the [`display`] panel builder, [`crate::io::input_caps`]) reads
/// these rather than re-stating the numbers.
#[cfg(feature = "cores3")]
pub const SCREEN_W: u16 = 320;
#[cfg(feature = "cores3")]
pub const SCREEN_H: u16 = 240;
#[cfg(feature = "fire27")]
pub const SCREEN_W: u16 = 320;
#[cfg(feature = "fire27")]
pub const SCREEN_H: u16 = 240;

/// Initialise esp-hal at max CPU clock. Heap setup stays with the app
/// (sizing is application policy).
pub fn init() -> Peripherals {
    esp_hal::init(esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::max()))
}

/// Chip-level (board-independent) system resources: executor timers, software
/// interrupts, the second core, and the RTC domain (e.g. the production RWDT,
/// [`crate::io::watchdog`]).
pub struct SystemResources<'a> {
    pub sw_int: SoftwareInterruptControl<'a>,
    pub timer0_0: AnyTimer<'a>,
    pub timer0_1: AnyTimer<'a>,
    pub timer1_0: AnyTimer<'a>,
    pub timer1_1: AnyTimer<'a>,
    pub cpu_ctrl: CPU_CTRL<'a>,
    /// RTC peripheral — hardware watchdog backstop, RTC time.
    pub lpwr: LPWR<'a>,
}
