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
//! The recommended path is [`Spi2Parts::finish_sd`]: the BSP owns the ≥74-clock
//! SD power-up idle and hands back a pre-initialised, presence-resolved
//! [`PreparedCard`]. The app supplies only its SD driver plus retry/degrade
//! policy — no board detail, no pre-init loop:
//!
//! ```ignore
//! let (mut parts, card_cs) = board.spi2.into_parts(dma_rx_buf, dma_tx_buf)?;
//! // Display comes up UNCONDITIONALLY (works even with a dead/absent card).
//! // `CardPresence::ForceAbsent` reaches the SD-absent path with a card in slot.
//! let (display, prepared) = parts.finish_sd(card_cs, CardPresence::Detect).await?;
//! let mut sd = SdSpi::new(prepared.into_inner());
//! // bounded init retries; on failure, degrade (SD absent). Both real-absent
//! // and ForceAbsent fail here, on the same single degrade path.
//! ```
//!
//! The lower-level [`Spi2Parts::finish`] primitive (no pre-init, plain generic
//! `card_cs`) remains for callers that drive the exclusive `bus` themselves
//! before sharing it.
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
// Only `finish` consumes the display pins, and it needs the `display` feature;
// without it they are still held, keeping the pins owned at the levels
// `into_parts` drove them to.
#[cfg_attr(not(feature = "display"), allow(dead_code))]
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
    use embassy_sync::semaphore::{FairSemaphore, Semaphore};
    use embedded_hal::digital::{ErrorType, OutputPin};
    use esp_hal::{
        dma::{DmaRxBuf, DmaTxBuf},
        gpio::{Level, Output, OutputConfig},
        spi::master::Spi,
    };
    use esp_sync::RawMutex;
    use lcd_async::interface::{Interface, SpiInterface};
    use static_cell::StaticCell;

    use super::{Spi2Parts, Spi2Resources, SpiBusType, display_config, sd_init_config};
    use crate::board::display::{self, Ili9342c};

    /// A device on the shared bus with a plain GPIO chip-select (the display).
    /// The display's SPI device, fairly arbitrated.
    ///
    /// Wrapped rather than raw so a panel transfer queues for the same permit
    /// the card takes. Making only the card fair would have moved the
    /// starvation to the display instead of removing it.
    pub type SpiDeviceType<'a> =
        FairSpiDevice<SpiDeviceWithConfig<'a, RawMutex, SpiBusType, Output<'a>>>;
    /// The SD-card device: CS is generic so the app can wrap it.
    pub type CardSpiDevice<CS> = SpiDeviceWithConfig<'static, RawMutex, SpiBusType, CS>;

    /// Whether the SD slot should behave as populated or be forced to degrade.
    ///
    /// `ForceAbsent` is a general force-degrade capability, **not** a HIL word:
    /// it makes the card device behave as an empty slot (chip-select never
    /// asserts), so the app's real `SdSpi::init()` runs and fails *authentically*
    /// — reaching the same graceful-degrade path as a physically empty slot,
    /// with a card inserted. Any HIL arming (an RTC one-shot, a `:nosd` verb)
    /// stays consumer-side; nothing HIL leaks into this surface.
    #[non_exhaustive]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum CardPresence {
        /// Drive the chip-select normally: a real card initialises; an empty
        /// slot degrades on its own.
        Detect,
        /// Freeze the chip-select deasserted so the card is never selected —
        /// `SdSpi::init()` then fails exactly like an empty slot.
        ForceAbsent,
    }

    /// Chip-select wrapper that can freeze the card *deasserted* to force the
    /// absent-card path (see [`CardPresence::ForceAbsent`]).
    ///
    /// SD chip-select is active-low, so `set_low` selects: when `frozen` it is
    /// suppressed (the card is never selected → MISO idles `0xFF` → authentic
    /// init failure), while `set_high` (deselect) is always honoured. It is
    /// carried *inside* [`PreparedCard`] because the freeze only bites where the
    /// CS is first **asserted** — downstream in the app's `SdSpi::init()` CMD0,
    /// not the CS-deasserted 74-clock pre-init. A single runtime-flag type (not
    /// a type-level marker) keeps [`Spi2Parts::finish_sd`] monomorphic across a
    /// runtime [`CardPresence`].
    pub struct PresenceCs<CS> {
        pin: CS,
        frozen: bool,
    }

    impl<CS: ErrorType> ErrorType for PresenceCs<CS> {
        type Error = CS::Error;
    }

    impl<CS: OutputPin> OutputPin for PresenceCs<CS> {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            if self.frozen {
                Ok(()) // suppress SELECT while forced-absent
            } else {
                self.pin.set_low()
            }
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.pin.set_high() // deselect is always honoured
        }
    }

    /// A card device on the shared SPI2 bus, pre-initialised by
    /// [`Spi2Parts::finish_sd`] (the ≥74-clock power-up idle has run) and
    /// presence-resolved. The BSP owns everything up to here with no SD-driver
    /// type in its graph; the app supplies only its SD driver:
    /// `SdSpi::new(prepared.into_inner())`.
    pub struct PreparedCard<CS: OutputPin> {
        bus: &'static Mutex<RawMutex, SpiBusType>,
        cs: PresenceCs<CS>,
    }

    /// A `SpiDevice` that queues for [`SPI2_FAIR`] before each transaction.
    ///
    /// The display holds the bus for a whole panel transfer, so without this the
    /// card's 1 ms-cadence busy-poll never gets in. With it, both users are
    /// served in arrival order and neither can monopolise the bus.
    pub struct FairSpiDevice<D> {
        inner: D,
    }

    impl<D> FairSpiDevice<D> {
        pub fn new(inner: D) -> Self {
            Self { inner }
        }
    }

    impl<'a, M, BUS, CS> FairSpiDevice<SpiDeviceWithConfig<'a, M, BUS, CS>>
    where
        M: embassy_sync::blocking_mutex::raw::RawMutex,
        BUS: embassy_embedded_hal::SetConfig,
    {
        /// Re-configure the underlying device (e.g. raise the SD clock after
        /// init). Delegates, so callers keep the wrapper — and with it the
        /// fairness — instead of having to unwrap to reach the setter.
        pub fn set_config(&mut self, config: BUS::Config) {
            self.inner.set_config(config)
        }
    }

    impl<D: embedded_hal_async::spi::ErrorType> embedded_hal_async::spi::ErrorType
        for FairSpiDevice<D>
    {
        type Error = D::Error;
    }

    impl<D: embedded_hal_async::spi::SpiDevice> embedded_hal_async::spi::SpiDevice
        for FairSpiDevice<D>
    {
        async fn transaction(
            &mut self,
            operations: &mut [embedded_hal_async::spi::Operation<'_, u8>],
        ) -> Result<(), Self::Error> {
            let queued = embassy_time::Instant::now();
            // A full waiter queue must NOT fall through to an unpermitted
            // transaction. `let _permit = acquire()` bound the RESULT, so on
            // `Err` this drove the bus with no mutual exclusion at all, while
            // the card may have been holding it. Drop the frame instead — the
            // card path degrades the same way and a lost frame costs a redraw.
            let Ok(_permit) = SPI2_FAIR.acquire(1).await else {
                warn!("SPI2: waiter queue full, display dropped a frame");
                return Ok(());
            };
            let waited = queued.elapsed();
            // This device applies its own config, so the bus is no longer set
            // up for the card; the next card acquire must restore it.
            BUS_IS_CARD.store(false, core::sync::atomic::Ordering::Release);
            let r = self.inner.transaction(operations).await;
            let total = queued.elapsed();
            if total.as_millis() > 100 {
                warn!(
                    "SPI2: display transaction {} ms ({} ms of it waiting for the bus)",
                    total.as_millis(),
                    waited.as_millis()
                );
            }
            r
        }
    }

    /// The SPI2 bus and the card's chip-select, held together.
    ///
    /// This exists because chip-select continuity is not expressible from
    /// outside the BSP. `SpiDevice::transaction` asserts CS on entry and
    /// deasserts on exit, and one SD command is several transactions — the
    /// `0xFE` data-token wait is unbounded, so it cannot be folded into a single
    /// fixed operation list. A driver holding only a `SpiDevice` therefore drops
    /// CS mid-command no matter how carefully it locks.
    ///
    /// The BSP owns both halves, so it can hand them out together: take this
    /// guard once per command, assert CS yourself, and CS stays low for the
    /// whole exchange. Drop it between commands (and between busy-poll probes)
    /// so the display still gets the bus — a full-card erase must not freeze
    /// the UI.
    pub struct Spi2CardGuard<'a, CS: OutputPin> {
        bus: embassy_sync::mutex::MutexGuard<'a, RawMutex, SpiBusType>,
        cs: &'a mut PresenceCs<CS>,
        /// Held for its `Drop`: returning the permit is what releases our turn.
        _permit: embassy_sync::semaphore::SemaphoreReleaser<
            'a,
            FairSemaphore<RawMutex, SPI2_WAITERS>,
        >,
    }

    impl<CS: OutputPin> Spi2CardGuard<'_, CS> {
        /// The bus and the chip-select, borrowed together for one command.
        pub fn split(&mut self) -> (&mut SpiBusType, &mut PresenceCs<CS>) {
            (&mut self.bus, self.cs)
        }
    }

    impl<CS: OutputPin> Drop for Spi2CardGuard<'_, CS> {
        /// Deselect the card as the bus is released.
        ///
        /// These are the same event: leaving CS low after the guard drops would
        /// leave the card selected while another master owns the bus, which is
        /// worse than the mid-command CS drops this guard exists to remove. It
        /// also lets the driver assert CS once per command and then use `?`
        /// freely — every early return releases the card by unwinding here.
        fn drop(&mut self) {
            let _ = self.cs.set_high();
        }
    }

    impl<CS: OutputPin> PreparedCard<CS> {
        /// Take the bus for ONE command, with the chip-select in hand.
        ///
        /// Released on drop. Hold it across a command; drop it between
        /// commands and between busy probes.
        pub async fn acquire(&mut self) -> Option<Spi2CardGuard<'_, CS>> {
            // Permit FIRST, then the mutex. Arrival order decides who goes
            // next; the mutex is uncontended by the time we take it.
            //
            // `None`, not a panic, when the waiter queue is full: a library
            // crate must never `panic!` (conventions/rust-code.md §6), and this
            // is SD/bus code where the house rule is warn-and-degrade — a full
            // queue must cost logging, never the regulator.
            let permit = match SPI2_FAIR.acquire(1).await {
                Ok(p) => p,
                Err(_) => {
                    warn!("SPI2: waiter queue full, card could not take the bus");
                    return None;
                }
            };
            Some(Spi2CardGuard {
                bus: self.bus.lock().await,
                cs: &mut self.cs,
                _permit: permit,
            })
        }

        /// The presence-resolved card `SpiDevice`, for drivers that do not need
        /// CS held across a command.
        ///
        /// Retained for existing callers. Prefer [`Self::acquire`]: a driver
        /// built on this cannot keep CS asserted across the data-token wait,
        /// because each `SpiDevice` call deasserts it.
        ///
        /// It IS still fairly arbitrated — the device is wrapped so every
        /// transaction queues for the same permit the display takes. An earlier
        /// version handed back the raw `SpiDeviceWithConfig`, which silently
        /// left the card outside the arbiter: exactly the half-fair
        /// configuration this change exists to remove, reachable through the
        /// path the module docs recommend for compatibility.
        pub fn into_inner(self) -> FairSpiDevice<CardSpiDevice<PresenceCs<CS>>> {
            FairSpiDevice::new(SpiDeviceWithConfig::new(self.bus, self.cs, sd_init_config()))
        }
    }

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

    /// Display-only DC pin: a plain [`Output`] on **both** boards. On CoreS3
    /// this is GPIO35 configured as a real output (which routes the pad) — NOT
    /// [`Gpio35Dc`](crate::board::cores3::Gpio35Dc), whose register-level mux
    /// relies on `with_miso()` having routed the pad and would otherwise leave
    /// it unrouted (black screen). Used by [`Spi2Resources::into_display_only`].
    pub type DisplayOnlyInterface = SpiInterface<SpiDeviceType<'static>, Output<'static>>;

    /// CoreS3 display-only panel: no GPIO reset (AW9523B pulses it).
    #[cfg(feature = "cores3")]
    pub type DisplayOnlyType = Ili9342c<DisplayOnlyInterface>;
    /// Fire27 display-only panel: hardware reset pin.
    #[cfg(feature = "fire27")]
    pub type DisplayOnlyType = Ili9342c<DisplayOnlyInterface, Output<'static>>;

    // Both boards' reset pins are infallible (`NoResetPin` / esp-hal `Output`).
    pub type DisplayOnlyInitError =
        lcd_async::InitError<<DisplayOnlyInterface as Interface>::Error, core::convert::Infallible>;

    /// A display brought up on the shared SPI2 DMA bus with **no** SD-card path
    /// ([`Spi2Resources::into_display_only`]). For LVGL and any DMA display-only
    /// app that does not touch the SD slot.
    pub struct DisplayBus {
        pub display: DisplayOnlyType,
        /// Backlight pin (GPIO32) — driven high by `into_display_only` once the
        /// panel init succeeds; the caller keeps it alive.
        #[cfg(feature = "fire27")]
        pub backlight: Output<'static>,
    }

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

    /// Waiter slots: the card and the display, plus slack so a mid-handover
    /// overlap cannot hit the `MaxWaiters` path.
    const SPI2_WAITERS: usize = 4;

    /// Fair arbiter in FRONT of the bus mutex.
    ///
    /// The mutex is still required — `SpiDeviceWithConfig` takes one — but it is
    /// not FIFO, and the two users are asymmetric: an SD busy-poll offers the
    /// bus up every 1 ms and asks for it straight back, while a display flush
    /// holds it for an entire panel transfer (15-30 ms, longer on fire27 with
    /// LVGL in SPI PSRAM). The flush therefore won repeatedly and the poll
    /// starved: an erase the card had long finished took seconds to be
    /// *observed* as finished, and fire27 `:format` drifted 440 ms -> 9434 ms
    /// while cores3, which renders faster, sat unchanged at ~2850 ms.
    ///
    /// Serving permits in arrival order makes both waits structural rather than
    /// statistical: the poll waits at most one display transfer, the display at
    /// most one SD command. Every user takes a permit BEFORE locking the mutex,
    /// so the mutex is only ever held by the permit holder.
    static SPI2_FAIR: FairSemaphore<RawMutex, SPI2_WAITERS> = FairSemaphore::new(1);

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
            let (driver, bus) = self.finish_bus().await?;
            Ok((driver, SpiDeviceWithConfig::new(bus, card_cs, sd_init_config())))
        }

        /// Display bring-up, yielding the SHARED bus rather than a composed
        /// device.
        ///
        /// Split out of [`Self::finish`] so [`Self::finish_sd`] can keep the bus
        /// and the chip-select as separate pieces — which is what lets a card
        /// driver hold CS across a whole command. Not generic over `CS`: the
        /// display path never needed to be.
        async fn finish_bus(
            self,
        ) -> Result<(DisplayDriver, &'static Mutex<RawMutex, SpiBusType>), DisplayInitError> {
            let bus = SPI_BUS.init(Mutex::new(self.bus));
            // Wrapped so every panel transfer queues for the SAME permit the
            // card takes. Fairness on one side only would just move the
            // starvation to the other.
            let display_device =
                FairSpiDevice::new(SpiDeviceWithConfig::new(bus, self.display_cs, display_config()));

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

            Ok((driver, bus))
        }

        /// Publish-safe full SD bring-up, layered on [`finish`].
        ///
        /// Runs the mandatory ≥74-clock power-up idle on the still-exclusive
        /// bus (chip-select deasserted), brings the display up unconditionally
        /// (see [`finish`]), and hands back a presence-resolved
        /// [`PreparedCard`]. The app owns only the final
        /// `SdSpi::new(prepared.into_inner()).init()` plus its retry/degrade
        /// policy — no SD-driver type enters the BSP graph.
        ///
        /// [`CardPresence::ForceAbsent`] reaches the app's normal SD-absent
        /// degrade path with a card inserted: the freeze takes effect at the
        /// first CS assert inside `SdSpi::init()`, so real-absent and
        /// forced-absent share one degrade path.
        pub async fn finish_sd<CS>(
            mut self,
            card_cs: CS,
            presence: CardPresence,
        ) -> Result<(DisplayDriver, PreparedCard<CS>), DisplayInitError>
        where
            CS: OutputPin,
        {
            // ≥74 clock cycles with CS deasserted (card_cs starts High) and DI
            // high — the SD power-up idle, per the SD-SPI spec. Done on the
            // still-exclusive bus before `finish` shares it; presence-independent
            // (the freeze only bites at the first CS assert, downstream). Not
            // via `sdspi::sd_init` — that fork must not enter the BSP graph.
            //
            // `SpiDmaBus::write` here is the inherent BLOCKING method: it returns
            // `Result`, not a `Future`, and drives the DMA transfer to completion
            // (~200 µs at 400 kHz for this one-time idle). Load-bearing: if `bus`
            // ever becomes a type whose `write` yields a future, this must be
            // `.await`ed or the idle would silently never run. HIL-validated on
            // both GDMA (CoreS3) and PDMA (Fire27) — the 10-byte / 80-clock write
            // does not wedge the ESP32 PDMA TX path.
            if let Err(e) = self.bus.write(&[0xFF; 10]) {
                warn!("SD power-up idle clock failed: {e:?}");
            }
            let card_cs = PresenceCs {
                pin: card_cs,
                frozen: matches!(presence, CardPresence::ForceAbsent),
            };
            let (driver, bus) = self.finish_bus().await?;
            Ok((driver, PreparedCard { bus, cs: card_cs }))
        }
    }

    #[cfg(feature = "cores3")]
    impl Spi2Resources<'static> {
        /// Bring up the display **only** on a descriptor-backed `SpiDmaBus`, with
        /// no SD-card path. The DMA buffers are supplied by the app (TX sized to
        /// the display stripe; RX unused by a write-only panel). DC is a plain
        /// `Output` on GPIO35 — a configured output routes the pad, unlike
        /// [`Gpio35Dc`](crate::board::cores3::Gpio35Dc) which needs `with_miso`.
        ///
        /// The panel must already be out of reset: call
        /// [`power_display_reset`](crate::board::cores3::power_display_reset)
        /// first (AW9523B `LCD_RST` pulse + AXP2101 backlight).
        pub async fn into_display_only(
            self,
            dma_rx_buf: DmaRxBuf,
            dma_tx_buf: DmaTxBuf,
        ) -> Result<DisplayBus, DisplayOnlyInitError> {
            // No `.with_miso()`: this path never reads the SD card, so GPIO35 is
            // free to be a plain DC output (below).
            let spi = Spi::new(self.spi2, display_config())
                .expect("SPI2 display config")
                .with_sck(self.sck)
                .with_mosi(self.mosi)
                .with_dma(self.spi2_dma)
                .with_buffers(dma_rx_buf, dma_tx_buf)
                .into_async();
            let bus = SPI_BUS.init(Mutex::new(spi));
            let display_cs = Output::new(self.display_cs, Level::High, OutputConfig::default());
            let dc = Output::new(self.miso_dc, Level::Low, OutputConfig::default());
            // Fair-wrapped like the shared path. These entry points are
            // display-only today, so nothing contends — but leaving one raw
            // would mean the place that skips the arbiter is the one nobody
            // re-checks when a second bus user appears.
            let device =
                FairSpiDevice::new(SpiDeviceWithConfig::new(bus, display_cs, display_config()));
            let di = SpiInterface::new(device, dc);
            let display = display::init_ili9342c(di).await?;
            Ok(DisplayBus { display })
        }
    }

    #[cfg(feature = "fire27")]
    impl Spi2Resources<'static> {
        /// Bring up the display **only** on a descriptor-backed `SpiDmaBus`, with
        /// no SD-card path. The DMA buffers are supplied by the app (TX sized to
        /// the display stripe; RX unused by a write-only panel). The panel is
        /// reset via its GPIO RST pin; the backlight (GPIO32) is driven high on
        /// success and returned in [`DisplayBus`] for the caller to keep alive.
        pub async fn into_display_only(
            self,
            dma_rx_buf: DmaRxBuf,
            dma_tx_buf: DmaTxBuf,
        ) -> Result<DisplayBus, DisplayOnlyInitError> {
            let spi = Spi::new(self.spi2, display_config())
                .expect("SPI2 display config")
                .with_sck(self.sck)
                .with_mosi(self.mosi)
                .with_miso(self.miso)
                .with_dma(self.spi2_dma)
                .with_buffers(dma_rx_buf, dma_tx_buf)
                .into_async();
            let bus = SPI_BUS.init(Mutex::new(spi));
            let display_cs = Output::new(self.display_cs, Level::High, OutputConfig::default());
            let dc = Output::new(self.display_dc, Level::Low, OutputConfig::default());
            let rst = Output::new(self.display_rst, Level::Low, OutputConfig::default());
            let mut backlight = Output::new(self.display_bl, Level::Low, OutputConfig::default());
            // Fair-wrapped like the shared path. These entry points are
            // display-only today, so nothing contends — but leaving one raw
            // would mean the place that skips the arbiter is the one nobody
            // re-checks when a second bus user appears.
            let device =
                FairSpiDevice::new(SpiDeviceWithConfig::new(bus, display_cs, display_config()));
            let di = SpiInterface::new(device, dc);
            let display = display::init_ili9342c_with_reset(di, rst).await?;
            backlight.set_high();
            Ok(DisplayBus { display, backlight })
        }
    }
}
