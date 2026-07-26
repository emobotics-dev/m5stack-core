// SPDX-License-Identifier: MIT OR Apache-2.0
//! M5Stack CoreS3 board-level bring-up sequences and pin wiring.
//!
//! # Internal I²C bus (`I2C_SYS`)
//!
//! One shared bus on **GPIO12 (SDA) / GPIO11 (SCL)** carries seven devices; the
//! BSP drives the first three, the rest are listed for completeness:
//!
//! | Device   | Addr | Role                        | BSP driver          |
//! |----------|------|-----------------------------|---------------------|
//! | AXP2101  | 0x34 | PMIC / power rails / battery | [`Axp2101Driver`]   |
//! | AW9523B  | 0x58 | IO expander (LCD/touch reset)| [`Aw9523bDriver`]   |
//! | FT6336U  | 0x38 | capacitive touch            | `driver::ft6336u`   |
//! | BMI270   | 0x69 | 6-axis IMU                  | —                   |
//! | BM8563   | 0x51 | RTC                         | —                   |
//! | ES7210   | 0x40 | audio ADC (mic)             | —                   |
//! | AW88298  | 0x36 | speaker amp                 | —                   |
//!
//! # AXP2101 power rails (from `Sch_M5_CoreS3_v1.0`)
//!
//! | AXP2101 out | Net           | Spec        | Powers                                  |
//! |-------------|---------------|-------------|-----------------------------------------|
//! | DCDC1       | `VDD_3V3`     | 3.3 V / 2 A | **Core VDD** — ESP32-S3, **AW9523B**, **FT6336U touch**, LCD logic |
//! | DCDC3       | `VCC_3V3`     | 3.3 V / 2 A | Peripherals VDD                         |
//! | DLDO1       | `VCC_BL`      | 3.3 V       | LCD **backlight** (see [`BACKLIGHT_MV`]) |
//! | ALDO1       | `VDD_1V8`     | 1.8 V/300 mA| AW88298 PA DVDD                         |
//! | ALDO2       | `VDDA_3V3`    | 3.3 V/300 mA| ES7210 codec VDDP                       |
//! | ALDO3       | `VDDCAM_3V3`  | 3.3 V/300 mA| camera / codec VDDA                     |
//! | ALDO4       | `VDD_3V3_SD`  | 3.3 V/300 mA| microSD card VDD                        |
//! | BLDO1       | `AVDD`        | 2.8 V/300 mA| sensor VDDA                             |
//! | BLDO2       | `DVDD`        | 1.2 V/300 mA| sensor VDD                              |
//!
//! **Reset gating (important):** the **AW9523B `RSTN` is wired to `AXP_PG`**
//! (AXP2101 power-good) and has an internal ~100 kΩ pull-*low* — so the expander
//! is held in reset until the AXP asserts power-good. It in turn drives
//! `LCD_RST` / `TOUCH_RST`, so panel + touch bring-up all chain off it. See
//! [`power_display_reset`] for the cold-boot readiness handling.

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

use embassy_time::{Duration, Timer};

use crate::board::SystemResources;
use crate::board::spi2::Spi2Resources;
use crate::driver::aw9523b::{Aw9523bDriver, Aw9523bResources};
use crate::driver::axp2101::Axp2101Driver;
use crate::io::shared_i2c::SharedI2cBus;

/// AXP2101 PMIC I2C address on CoreS3.
const AXP2101_ADDR: u8 = 0x34;
/// Display backlight rail: AXP2101 DLDO1 @ 3.3 V.
const BACKLIGHT_MV: u16 = 3300;

/// AW9523B post-power-on I²C-ready time — the datasheet's "minimal wait time for
/// I2C communication is 5 ms".
const AW9523B_POR_MS: u64 = 5;
/// Deterministic settle before the first AW9523B access: 200% of the POR time.
/// On CoreS3 the expander's `RSTN` has an internal ~100 kΩ pull-*low* and is
/// wired to `AXP_PG` (AXP2101 power-good), so it is held in reset until the AXP
/// asserts power-good; at cold power-on `VDD_3V3` is already up (it powers the
/// running ESP32) but `AXP_PG` may lag, leaving the expander briefly in reset.
/// Waiting first — rather than poking the bus and retrying — avoids the premature
/// `AcknowledgeCheckFailed(Address)`, each of which fires one chip-wide I2C clock
/// reset.
const AW9523B_SETTLE_MS: u64 = 2 * AW9523B_POR_MS;
/// Fallback if the expander is still not ready after the settle. This should not
/// happen in normal operation, so each attempt logs a warning; bounded, with a
/// final failure logged as an error (best-effort — bring-up continues).
const AW9523B_FALLBACK_TRIES: u32 = 5;
const AW9523B_FALLBACK_DELAY_MS: u64 = 5;

/// Fault-injection predicate for the `test-fault-inject` feature: reports the
/// AW9523B "not ready" for the first `M5_TEST_AW_NACK` post-settle attempts (a
/// build-time env var), so the cold-boot fallback retry can be regression-tested
/// on the bench without a real power cycle. Absent from production builds.
#[cfg(feature = "test-fault-inject")]
fn aw_fault_inject_active(tries: u32) -> bool {
    match option_env!("M5_TEST_AW_NACK") {
        Some(s) => tries < s.parse::<u32>().unwrap_or(0),
        None => false,
    }
}

/// Power + display bring-up over the shared I2C bus: AW9523B IO-expander
/// (`LCD_RST` + touch reset) then AXP2101 PMIC backlight (DLDO1 @ 3.3 V).
///
/// A [`AW9523B_SETTLE_MS`] wait precedes the first AW9523B access so the expander
/// is out of reset (its `RSTN` follows `AXP_PG`) before we touch the bus — this
/// removes the intermittent cold-boot `AcknowledgeCheckFailed(Address)` that
/// otherwise fired one chip-wide I2C clock reset at every power-on. A bounded
/// warn-on-failure retry backs it up in case the settle was not enough.
///
/// Best-effort — each sub-step logs and continues on error, so a flaky chip
/// can't block the display/control loop from coming up. Caller-driven so the
/// caller owns the *when* and the *which core*: this MUST run on the core that
/// owns the I2C IRQ (APP core on the regulator — see its `irq-core-binding`
/// doc), and the caller signals display-readiness after it returns
/// (`LCD_RST` must precede display init).
pub async fn power_display_reset(i2c: &'static SharedI2cBus) {
    let mut aw = Aw9523bDriver::new(Aw9523bResources { i2c });
    // Deterministic settle for the cold-boot reset-release window (RSTN = AXP_PG)
    // before the first bus access — see AW9523B_SETTLE_MS.
    Timer::after(Duration::from_millis(AW9523B_SETTLE_MS)).await;
    let mut tries = 0u32;
    loop {
        // Test hook (feature `test-fault-inject`, absent in production builds):
        // simulate the AW9523B still being not-ready for the first
        // `M5_TEST_AW_NACK` post-settle attempts, so the fallback retry is
        // exercisable on the bench without a power cycle (which the rig can't do).
        #[cfg(feature = "test-fault-inject")]
        if aw_fault_inject_active(tries) {
            tries += 1;
            if tries > AW9523B_FALLBACK_TRIES {
                error!("AW9523B not ready (fault-inject) after {} retries", AW9523B_FALLBACK_TRIES);
                break;
            }
            warn!(
                "[test-fault-inject] AW9523B not-ready simulated; retry {}/{}",
                tries, AW9523B_FALLBACK_TRIES
            );
            Timer::after(Duration::from_millis(AW9523B_FALLBACK_DELAY_MS)).await;
            continue;
        }
        match aw.init().await {
            Ok(()) => {
                if tries > 0 {
                    info!("AW9523B ready after settle + {} retries", tries);
                }
                break;
            }
            Err(e) => {
                tries += 1;
                if tries > AW9523B_FALLBACK_TRIES {
                    error!(
                        "AW9523B not ready after {} ms settle + {} retries: {:?}",
                        AW9523B_SETTLE_MS, AW9523B_FALLBACK_TRIES, e
                    );
                    break;
                }
                warn!(
                    "AW9523B not ready after settle; retry {}/{}: {:?}",
                    tries, AW9523B_FALLBACK_TRIES, e
                );
                Timer::after(Duration::from_millis(AW9523B_FALLBACK_DELAY_MS)).await;
            }
        }
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
    /// M5-Bus pin 20 (`BUS_PA_SCL`) → GPIO1.
    pub gpio1: AnyPin<'a>,
    /// M5-Bus pin 10 (`BUS_PB_OUT`) → GPIO9.
    pub gpio9: AnyPin<'a>,
    /// M5-Bus pin 2 (`BUS_ADC1`) → GPIO10.
    pub gpio10: AnyPin<'a>,
    /// M5-Bus pin 8 (`BUS_G5`) → GPIO5.
    pub gpio5: AnyPin<'a>,
    /// M5-Bus pin 21 (`BUS_G6`) → GPIO6.
    pub gpio6: AnyPin<'a>,
    /// M5-Bus pin 22 (`BUS_G7`) → GPIO7.
    pub gpio7: AnyPin<'a>,
    /// M5-Bus pin 13 (`BUS_U0RXD`) → GPIO44 — the ROM console's RX pin,
    /// unused on CoreS3 (console is USB-Serial-JTAG). Assign as an input:
    /// a ROM boot-log burst on the TX side (`gpio43`) can then never
    /// contend with a driven line here.
    pub gpio44: AnyPin<'a>,
    /// M5-Bus pin 14 (`BUS_U0TXD`) → GPIO43 — the ROM console's TX pin,
    /// unused on CoreS3. Assign as an output; see [`Self::gpio44`].
    pub gpio43: AnyPin<'a>,
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
    /// SK6812 LED bars data pin (M-Bus pin 23 → GPIO13 on CoreS3), driven over
    /// RMT ([`crate::driver::sk6812::Sk6812Driver`]). The M5GO bottom's 5 V LED
    /// rail is gated by the AW9523B — see `Aw9523bDriver::enable_bus_5v`.
    pub sk6812: AnyPin<'static>,
    /// External SPI PSRAM (~8 MB) — feed to `mem::init_psram_heap` (the heap
    /// region; sizing/usage is application policy). [feature `psram`]
    pub psram: esp_hal::peripherals::PSRAM<'static>,
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
            sk6812: AnyPin::from(peripherals.GPIO13),
            psram: peripherals.PSRAM,
            m5bus: M5Bus {
                gpio1: AnyPin::from(peripherals.GPIO1),
                gpio9: AnyPin::from(peripherals.GPIO9),
                gpio10: AnyPin::from(peripherals.GPIO10),
                gpio5: AnyPin::from(peripherals.GPIO5),
                gpio6: AnyPin::from(peripherals.GPIO6),
                gpio7: AnyPin::from(peripherals.GPIO7),
                gpio44: AnyPin::from(peripherals.GPIO44),
                gpio43: AnyPin::from(peripherals.GPIO43),
            },
            system,
        }
    }
}
