// SPDX-License-Identifier: MIT OR Apache-2.0
//! M5Stack CoreS3 board-level bring-up sequences and pin wiring.

use embedded_hal::digital::{ErrorType, OutputPin};
use esp_hal::{
    Blocking,
    dma::DmaChannelConvert,
    gpio::AnyPin,
    i2c::master::{BusTimeout, Config as I2cConfig, I2c, SoftwareTimeout},
    interrupt::software::SoftwareInterruptControl,
    peripherals::{BT, PCNT, Peripherals, RMT, USB_DEVICE, WIFI},
    spi::master::AnySpi,
    time::Rate,
    timer::{AnyTimer, timg::TimerGroup},
};

use crate::board::SystemResources;
use crate::board::spi2::Spi2Resources;
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

// --- GPIO35 MISO/DC mux -----------------------------------------------------

/// GPIO35 bit position in the GPIO1 (pins 32–) register bank.
const GPIO35_BIT: u32 = 1 << (35 - 32);

/// Zero-sized display-DC "pin" that drives GPIO35 via direct register writes.
///
/// On the CoreS3, GPIO35 is hardware-shared between SPI2 MISO (SD-card reads)
/// and display DC (command/data select). This type does NOT own the pin — the
/// SPI peripheral owns it for MISO input (`with_miso`), which also routes the
/// pad through the GPIO matrix. Each `set_low`/`set_high` re-enables the
/// output driver, so it composes with [`gpio35_disable_output`] (called after
/// every SD access window).
///
/// Two valid configurations for GPIO35 as DC:
/// 1. **No SD use** (simple display apps): configure GPIO35 as a plain
///    [`Output`](esp_hal::gpio::Output) instead — a bare register hack without
///    any pad routing leaves the pad unrouted and DC never toggles.
/// 2. **Display + SD shared** ([`super::spi2`]): SPI owns the pad as MISO and
///    DC writes go through this type.
///
/// Safety of the mux: the display and the SD card must share one SPI-bus
/// mutex within a single task, with no `.await` between a DC write and the
/// SPI transfer — otherwise an SD operation could preempt mid-command.
pub struct Gpio35Dc;

impl ErrorType for Gpio35Dc {
    type Error = core::convert::Infallible;
}

impl OutputPin for Gpio35Dc {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        unsafe {
            let gpio = &*esp_hal::peripherals::GPIO::PTR;
            gpio.out1_w1tc().write(|w| w.bits(GPIO35_BIT));
            gpio.enable1_w1ts().write(|w| w.bits(GPIO35_BIT));
        }
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        unsafe {
            let gpio = &*esp_hal::peripherals::GPIO::PTR;
            gpio.out1_w1ts().write(|w| w.bits(GPIO35_BIT));
            gpio.enable1_w1ts().write(|w| w.bits(GPIO35_BIT));
        }
        Ok(())
    }
}

/// Disable the GPIO35 output driver so the pad returns to high-impedance
/// input (SPI2 MISO). Must be called before any SD-card SPI operation — both
/// at bring-up (after display init; [`super::spi2`] does it) and per-op at
/// runtime (pass this as the block-device handler's pre-op hook).
pub fn gpio35_disable_output() {
    unsafe {
        let gpio = &*esp_hal::peripherals::GPIO::PTR;
        gpio.enable1_w1tc().write(|w| w.bits(GPIO35_BIT));
    }
}

// --- Pin wiring ---------------------------------------------------------------

/// External M5-Bus pins the BSP does not claim for on-board functions. What
/// hangs off them (1-Wire sensors, RPM pickups, ...) is application wiring.
/// Not exhaustive — extend as needed.
pub struct M5Bus<'a> {
    /// M5-Bus pin 5 → GPIO1.
    pub gpio1: AnyPin<'a>,
    /// M5-Bus pin 10 → GPIO9.
    pub gpio9: AnyPin<'a>,
}

/// The CoreS3's peripherals, split into board-fact groups. See
/// [`Board::split`].
pub struct Board {
    /// Display + SD-card shared SPI2 bus (see [`super::spi2`]).
    pub spi2: Spi2Resources<'static>,
    /// Internal I2C bus (SDA=GPIO12, SCL=GPIO11) shared by the AXP2101 PMIC
    /// (0x34), AW9523B expander (0x58) and FT6336U touch (0x38).
    ///
    /// Returned BLOCKING: defer `into_async()` to the core that runs the I2C
    /// tasks — `into_async()` binds the I2C peripheral IRQ to the calling
    /// core, and on a BLE app it must stay off the core running RWBLE's
    /// level-1 IRQ (cumulative IRQ blocking otherwise desyncs the BLE
    /// controller blob and wedges that core's executor).
    pub i2c0: I2c<'static, Blocking>,
    pub bt: BT<'static>,
    pub wifi: WIFI<'static>,
    /// USB-Serial-JTAG — console + serial-cmd
    /// ([`crate::io::serial_cmd::SerialCmdResources`]).
    pub usb_device: USB_DEVICE<'static>,
    /// RMT — e.g. the 1-Wire driver ([`crate::io::ow_temp::OnewireResources`]).
    pub rmt: RMT<'static>,
    /// PCNT — e.g. pulse counting ([`crate::io::rpm::RpmResources`]).
    pub pcnt: PCNT<'static>,
    pub m5bus: M5Bus<'static>,
    pub system: SystemResources<'static>,
}

impl Board {
    /// Wire the CoreS3 pin map. Call once with the output of
    /// [`crate::board::init`] (or `esp_hal::init`); heap setup stays with the
    /// app (sizing is application policy).
    pub fn split(peripherals: Peripherals) -> Self {
        let spi2 = Spi2Resources {
            spi2: AnySpi::from(peripherals.SPI2),
            spi2_dma: peripherals.DMA_CH0.degrade(),
            sck: AnyPin::from(peripherals.GPIO36),
            mosi: AnyPin::from(peripherals.GPIO37),
            miso_dc: AnyPin::from(peripherals.GPIO35),
            display_cs: AnyPin::from(peripherals.GPIO3),
            card_cs: AnyPin::from(peripherals.GPIO4),
        };

        let i2c0 = I2c::new(
            peripherals.I2C0,
            I2cConfig::default()
                .with_frequency(Rate::from_khz(400))
                .with_timeout(BusTimeout::BusCycles(20))
                // Bound async transactions: without a software timeout the
                // deadline is None, so a stuck/NACKing transaction loops
                // forever in esp-hal's `check_all_commands_done` yield_now
                // spin. On an InterruptExecutor that spin pins the core,
                // masking the very completion IRQ that would end it — the
                // whole executor wedges (HIL-proven on S3, 2026-06-02). A
                // finite deadline makes a stuck op return Err instead.
                .with_software_timeout(SoftwareTimeout::Transaction(
                    esp_hal::time::Duration::from_millis(25),
                )),
        )
        .expect("I2C0 init failed")
        .with_sda(AnyPin::from(peripherals.GPIO12))
        .with_scl(AnyPin::from(peripherals.GPIO11));

        let tg0 = TimerGroup::new(peripherals.TIMG0);
        let tg1 = TimerGroup::new(peripherals.TIMG1);
        let system = SystemResources {
            sw_int: SoftwareInterruptControl::new(peripherals.SW_INTERRUPT),
            timer0_0: AnyTimer::from(tg0.timer0),
            timer0_1: AnyTimer::from(tg0.timer1),
            timer1_0: AnyTimer::from(tg1.timer0),
            timer1_1: AnyTimer::from(tg1.timer1),
            cpu_ctrl: peripherals.CPU_CTRL,
            lpwr: peripherals.LPWR,
        };

        Board {
            spi2,
            i2c0,
            bt: peripherals.BT,
            wifi: peripherals.WIFI,
            usb_device: peripherals.USB_DEVICE,
            rmt: peripherals.RMT,
            pcnt: peripherals.PCNT,
            m5bus: M5Bus {
                gpio1: AnyPin::from(peripherals.GPIO1),
                gpio9: AnyPin::from(peripherals.GPIO9),
            },
            system,
        }
    }
}
