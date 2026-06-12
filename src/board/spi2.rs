// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared SPI2 bus: on-board ILI9342C display + SD-card slot.
//!
//! Both boards route the display and the SD slot over the same SPI2 bus, so
//! their bring-up is coupled and ordering-sensitive. This module owns that
//! hardware knowledge; the SD-card *driver* stays with the application (the
//! `sdspi` crate is not yet on crates.io) and connects through the generic
//! chip-select + `SpiDevice` returned by [`Spi2Parts::finish`].
//!
//! # Bring-up sequence (application side)
//!
//! ```ignore
//! let (mut parts, card_cs) = board.spi2.into_parts(dma_rx_buf, dma_tx_buf)?;
//! // Optionally wrap card_cs (e.g. a HIL "no SD card" kill-switch).
//! let mut card_cs = card_cs;
//! // SD pre-init: cards need >= 74 clock cycles without CS asserted. MUST be
//! // bounded — a dead/absent card must never block the display bring-up:
//! let mut sd_pre_ok = false;
//! for attempt in 0..5 {
//!     match sdspi::sd_init(&mut parts.bus, &mut card_cs).await {
//!         Ok(_) => { sd_pre_ok = true; break; }
//!         Err(e) => { warn!("sd_init attempt {}: {:?}", attempt, e);
//!                     Timer::after_millis(10).await; }
//!     }
//! }
//! // Display comes up UNCONDITIONALLY — it must work even with a dead card:
//! let (display, card_device) = parts.finish(card_cs).await?;
//! if sd_pre_ok {
//!     let mut sd = SdSpi::new(card_device, Delay);
//!     // bounded init retries, then raise the device clock via set_config
//! }
//! ```
//!
//! # CoreS3: GPIO35 is shared between SPI2 MISO and display DC
//!
//! The SPI peripheral owns GPIO35 as MISO input (via `with_miso`); the display
//! DC is driven on the same pad through direct GPIO register writes
//! ([`Gpio35Dc`](crate::board::cores3::Gpio35Dc)). [`Spi2Parts::finish`] calls
//! [`gpio35_disable_output`](crate::board::cores3::gpio35_disable_output)
//! after the display init so the pad returns to high-impedance MISO input —
//! without it `sd_card.init()` reads garbage on MISO and never completes. At
//! runtime, every SD operation must re-assert that mux (pass the same function
//! as the block-device handler's pre-op hook). This is safe only while the
//! display and the SD card share one task/bus mutex with no `.await` between a
//! DC write and the SPI transfer.
//!
//! # Display/SD task join order differs per chip — do not unify
//!
//! When the app joins the display flush loop and the SD block-device handler
//! in one task: on fire27 (ESP32/PDMA) the SD handler must be polled FIRST so
//! it grabs the bus mutex preferentially — the mirror order reliably wedges
//! the SD path. cores3 (ESP32-S3/GDMA) needs the opposite order. The two
//! chips' interrupt/DMA timing has opposite contention characteristics; don't
//! change either without re-validating both targets on hardware.

use esp_hal::{
    Async,
    dma::{DmaRxBuf, DmaTxBuf},
    gpio::{AnyPin, Level, Output, OutputConfig},
    spi::{
        Mode,
        master::{AnySpi, Config as SpiConfig, ConfigError, Spi, SpiDmaBus},
    },
    time::Rate,
};

/// The shared bus: descriptor-backed DMA SPI. A plain `Spi::into_async()`
/// flush goes "usr-stuck" after the first frame on the ESP32 PDMA path, so a
/// `SpiDmaBus` is required (the CoreS3 uses GDMA for the same reason).
pub type SpiBusType = SpiDmaBus<'static, Async>;

/// Bus base config: 400 kHz Mode 0 — the clock SD cards require during init.
/// The app raises the card device's clock (via `SetConfig`) after `init()`.
pub fn sd_init_config() -> SpiConfig {
    SpiConfig::default()
        .with_frequency(Rate::from_khz(400))
        .with_mode(Mode::_0)
}

/// Display device config: 40 MHz Mode 0.
pub fn display_config() -> SpiConfig {
    sd_init_config().with_frequency(Rate::from_khz(40_000))
}

/// SPI2 pins + units (CoreS3): SCK=GPIO36, MOSI=GPIO37, MISO/DC=GPIO35,
/// display CS=GPIO3, card CS=GPIO4, GDMA channel 0.
#[cfg(feature = "cores3")]
pub struct Spi2Resources<'a> {
    pub spi2: AnySpi<'a>,
    pub spi2_dma: esp_hal::dma::AnyGdmaChannel<'a>,
    pub sck: AnyPin<'a>,
    pub mosi: AnyPin<'a>,
    /// GPIO35 — SPI2 MISO (SD reads) **and** display DC via the register-level
    /// mux ([`crate::board::cores3::Gpio35Dc`]); see the module docs.
    pub miso_dc: AnyPin<'a>,
    pub display_cs: AnyPin<'a>,
    pub card_cs: AnyPin<'a>,
}

/// SPI2 pins + units (Fire27): SCK=GPIO18, MOSI=GPIO23, MISO=GPIO19,
/// display CS=GPIO14 / DC=GPIO27 / RST=GPIO33 / BL=GPIO32, card CS=GPIO4,
/// PDMA SPI2 channel.
#[cfg(feature = "fire27")]
pub struct Spi2Resources<'a> {
    pub spi2: AnySpi<'a>,
    pub spi2_dma: esp_hal::dma::AnySpiDmaChannel<'a>,
    pub sck: AnyPin<'a>,
    pub mosi: AnyPin<'a>,
    pub miso: AnyPin<'a>,
    pub display_cs: AnyPin<'a>,
    pub display_dc: AnyPin<'a>,
    pub display_rst: AnyPin<'a>,
    pub display_bl: AnyPin<'a>,
    pub card_cs: AnyPin<'a>,
}

/// The constructed bus + display pins, between [`Spi2Resources::into_parts`]
/// and [`Spi2Parts::finish`]. `bus` is still exclusive here — the window for
/// the app's SD pre-init (see the module docs).
pub struct Spi2Parts {
    /// Exclusive DMA bus (not yet shared) for `sdspi::sd_init`.
    pub bus: SpiBusType,
    display_cs: Output<'static>,
    #[cfg(feature = "fire27")]
    display_dc: Output<'static>,
    #[cfg(feature = "fire27")]
    display_rst: Output<'static>,
    #[cfg(feature = "fire27")]
    display_bl: Output<'static>,
}

#[cfg(feature = "cores3")]
impl Spi2Resources<'static> {
    /// Build the DMA bus and the CS outputs. The DMA buffers are supplied by
    /// the app — their sizing is application policy (SD block size for RX,
    /// display strip size for TX). Returns the card CS separately so the app
    /// can wrap it (e.g. a HIL "no SD card" kill-switch) before
    /// [`Spi2Parts::finish`].
    pub fn into_parts(
        self,
        dma_rx_buf: DmaRxBuf,
        dma_tx_buf: DmaTxBuf,
    ) -> Result<(Spi2Parts, Output<'static>), ConfigError> {
        let bus = Spi::new(self.spi2, sd_init_config())?
            .with_sck(self.sck)
            .with_mosi(self.mosi)
            .with_miso(self.miso_dc)
            .with_dma(self.spi2_dma)
            .with_buffers(dma_rx_buf, dma_tx_buf)
            .into_async();
        let display_cs = Output::new(self.display_cs, Level::High, OutputConfig::default());
        let card_cs = Output::new(self.card_cs, Level::High, OutputConfig::default());
        Ok((Spi2Parts { bus, display_cs }, card_cs))
    }
}

#[cfg(feature = "fire27")]
impl Spi2Resources<'static> {
    /// Build the DMA bus and the CS/DC/RST/BL outputs. The DMA buffers are
    /// supplied by the app — their sizing is application policy (SD block size
    /// for RX, display strip size for TX). Returns the card CS separately so
    /// the app can wrap it (e.g. a HIL "no SD card" kill-switch) before
    /// [`Spi2Parts::finish`]. The backlight starts LOW (no flicker during
    /// panel init); `finish` turns it on.
    pub fn into_parts(
        self,
        dma_rx_buf: DmaRxBuf,
        dma_tx_buf: DmaTxBuf,
    ) -> Result<(Spi2Parts, Output<'static>), ConfigError> {
        let bus = Spi::new(self.spi2, sd_init_config())?
            .with_sck(self.sck)
            .with_mosi(self.mosi)
            .with_miso(self.miso)
            .with_dma(self.spi2_dma)
            .with_buffers(dma_rx_buf, dma_tx_buf)
            .into_async();
        let display_cs = Output::new(self.display_cs, Level::High, OutputConfig::default());
        let card_cs = Output::new(self.card_cs, Level::High, OutputConfig::default());
        let display_dc = Output::new(self.display_dc, Level::Low, OutputConfig::default());
        let display_rst = Output::new(self.display_rst, Level::Low, OutputConfig::default());
        let display_bl = Output::new(self.display_bl, Level::Low, OutputConfig::default());
        Ok((
            Spi2Parts { bus, display_cs, display_dc, display_rst, display_bl },
            card_cs,
        ))
    }
}

// --- Device construction + display init (feature `display`) ----------------

#[cfg(feature = "display")]
pub use devices::*;

#[cfg(feature = "display")]
mod devices {
    use embassy_embedded_hal::shared_bus::asynch::spi::SpiDeviceWithConfig;
    use embassy_sync::mutex::Mutex;
    use esp_hal::gpio::Output;
    use esp_sync::RawMutex;
    use lcd_async::interface::{Interface, SpiInterface};
    use static_cell::StaticCell;

    use super::{SpiBusType, Spi2Parts, display_config, sd_init_config};
    use crate::board::display::{self, Ili9342c};

    /// A device on the shared bus with a plain GPIO chip-select (the display).
    pub type SpiDeviceType<'a> = SpiDeviceWithConfig<'a, RawMutex, SpiBusType, Output<'a>>;
    /// The SD-card device: CS is generic so the app can wrap it.
    pub type CardSpiDevice<CS> = SpiDeviceWithConfig<'static, RawMutex, SpiBusType, CS>;

    #[cfg(feature = "cores3")]
    pub type DisplayInterface =
        SpiInterface<SpiDeviceType<'static>, crate::board::cores3::Gpio35Dc>;
    #[cfg(feature = "fire27")]
    pub type DisplayInterface = SpiInterface<SpiDeviceType<'static>, Output<'static>>;

    /// CoreS3 panel: no GPIO reset (AW9523B pulses it; SPI SoftReset fallback).
    #[cfg(feature = "cores3")]
    pub type DisplayType = Ili9342c<DisplayInterface>;
    /// Fire27 panel: hardware reset pin.
    #[cfg(feature = "fire27")]
    pub type DisplayType = Ili9342c<DisplayInterface, Output<'static>>;

    pub type DisplayInitError =
        lcd_async::InitError<<DisplayInterface as Interface>::Error, core::convert::Infallible>;

    /// The initialised on-board display (plus, on Fire27, its backlight pin).
    pub struct DisplayDriver {
        pub display: DisplayType,
        #[cfg(feature = "fire27")]
        bl: Output<'static>,
    }

    #[cfg(feature = "fire27")]
    impl DisplayDriver {
        pub fn bl_on(&mut self) {
            self.bl.set_high();
        }

        pub fn bl_off(&mut self) {
            self.bl.set_low();
        }
    }

    static SPI_BUS: StaticCell<Mutex<RawMutex, SpiBusType>> = StaticCell::new();

    impl Spi2Parts {
        /// Share the bus, initialise the display, and build the SD-card
        /// device.
        ///
        /// The display comes up first and unconditionally — it must work even
        /// when the SD card is dead or absent, so the system keeps a UI. On
        /// CoreS3 this also restores GPIO35 to high-impedance MISO input
        /// afterwards (see the module docs); on Fire27 it turns the backlight
        /// on after a successful init.
        ///
        /// On CoreS3 the panel must be out of reset before this is called:
        /// wait for [`power_display_reset`](crate::board::cores3::power_display_reset)
        /// (AW9523B `LCD_RST` pulse) — bounded, so a wedged I2C init can't
        /// freeze the display task forever.
        pub async fn finish<CS>(
            self,
            card_cs: CS,
        ) -> Result<(DisplayDriver, CardSpiDevice<CS>), DisplayInitError> {
            let bus = SPI_BUS.init(Mutex::new(self.bus));
            let display_device =
                SpiDeviceWithConfig::new(bus, self.display_cs, display_config());

            #[cfg(feature = "cores3")]
            let driver = {
                let di = SpiInterface::new(display_device, crate::board::cores3::Gpio35Dc);
                let display = display::init_ili9342c(di).await?;
                // CRITICAL: the display init drove GPIO35 as DC (output).
                // Restore it to high-impedance MISO input now — otherwise the
                // app's `sd_card.init()` reads garbage on MISO and never
                // completes. Runtime SD ops must re-assert this per-op.
                crate::board::cores3::gpio35_disable_output();
                DisplayDriver { display }
            };

            #[cfg(feature = "fire27")]
            let driver = {
                let di = SpiInterface::new(display_device, self.display_dc);
                let display =
                    display::init_ili9342c_with_reset(di, self.display_rst).await?;
                let mut driver = DisplayDriver { display, bl: self.display_bl };
                driver.bl_on();
                driver
            };

            let card_device = SpiDeviceWithConfig::new(bus, card_cs, sd_init_config());
            Ok((driver, card_device))
        }
    }
}
