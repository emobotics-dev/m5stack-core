// SPDX-License-Identifier: MIT OR Apache-2.0
//! ILI9342C panel bring-up shared by both boards (feature `display`).
//!
//! The M5Stack Fire27 and CoreS3 use the same 320x240 ILI9342C panel with the
//! same controller options (inverted colors, BGR order); only the reset wiring
//! differs — Fire27 has a GPIO reset pin, the CoreS3 panel is reset via the
//! AW9523B expander (see [`super::cores3::power_display_reset`]) and
//! falls back to lcd-async's SPI `SoftReset` ([`NoResetPin`]), which is
//! harmless after the hardware pulse.
//!
//! Generic over the [`Interface`]: works with a plain shared `Spi` bus device
//! (the examples) as well as a descriptor-backed `SpiDmaBus` device
//! ([`super::spi2`]).

use core::convert::Infallible;

use embedded_hal::digital::OutputPin;
use lcd_async::{
    Builder, Display, InitError, NoResetPin,
    interface::Interface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
};

/// Panel resolution — the single per-target source in [`super`] (a board fact,
/// not display-driver-specific); re-exported here for the display API.
pub use super::{SCREEN_H, SCREEN_W};

/// The configured panel type: ILI9342C in RGB565 over `DI`.
pub type Ili9342c<DI, RST = NoResetPin> = Display<DI, ILI9342CRgb565, RST>;

fn builder<DI: Interface>(di: DI) -> Builder<DI, ILI9342CRgb565, NoResetPin> {
    Builder::new(ILI9342CRgb565, di)
        .invert_colors(ColorInversion::Inverted)
        .color_order(ColorOrder::Bgr)
        .display_size(SCREEN_W, SCREEN_H)
}

/// Initialise the panel without a reset pin (CoreS3). The hardware reset must
/// have been pulsed beforehand (AW9523B `LCD_RST`); init falls back to a SPI
/// `SoftReset` command.
pub async fn init_ili9342c<DI: Interface>(
    di: DI,
) -> Result<Ili9342c<DI>, InitError<DI::Error, Infallible>> {
    let mut delay = embassy_time::Delay;
    builder(di).init(&mut delay).await
}

/// Initialise the panel with a hardware reset pin (Fire27, RST=GPIO33).
pub async fn init_ili9342c_with_reset<DI: Interface, RST: OutputPin>(
    di: DI,
    rst: RST,
) -> Result<Ili9342c<DI, RST>, InitError<DI::Error, RST::Error>> {
    let mut delay = embassy_time::Delay;
    builder(di).reset_pin(rst).init(&mut delay).await
}
