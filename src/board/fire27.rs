// SPDX-License-Identifier: MIT OR Apache-2.0
//! M5Stack Fire v2.7 (ESP32) board-level pin wiring.

use esp_hal::{
    Blocking,
    dma::AnySpiDmaChannel,
    gpio::AnyPin,
    i2c::master::{BusTimeout, Config as I2cConfig, I2c},
    interrupt::software::SoftwareInterruptControl,
    peripherals::{BT, PCNT, Peripherals, RMT, UART0, WIFI},
    spi::master::AnySpi,
    time::Rate,
    timer::{AnyTimer, timg::TimerGroup},
};

use crate::board::SystemResources;
use crate::board::spi2::Spi2Resources;
use crate::io::buttons::ButtonResources;

/// External M5-Bus pins the BSP does not claim for on-board functions. What
/// hangs off them (1-Wire sensors, RPM pickups, ...) is application wiring.
/// Not exhaustive — extend as needed.
pub struct M5Bus<'a> {
    pub gpio5: AnyPin<'a>,
    pub gpio26: AnyPin<'a>,
}

/// The Fire27's peripherals, split into board-fact groups. See
/// [`Board::split`].
pub struct Board {
    /// Display + SD-card shared SPI2 bus (see [`super::spi2`]).
    pub spi2: Spi2Resources<'static>,
    /// Internal I2C bus (SDA=GPIO21, SCL=GPIO22) — IP5306 fuel gauge (0x75,
    /// M5GO bottom) and any Grove/external I2C devices.
    ///
    /// Returned BLOCKING: defer `into_async()` to the core that runs the I2C
    /// tasks — `into_async()` binds the I2C peripheral IRQ to the calling
    /// core, and on a BLE app it must stay off the core running RWBLE's
    /// level-1 IRQ (cumulative IRQ blocking otherwise desyncs the BLE
    /// controller blob and wedges that core's executor).
    pub i2c0: I2c<'static, Blocking>,
    /// Front-panel buttons A/B/C (GPIO39/38/37, active-low). Use
    /// [`ButtonResources::into_buttons`] (feature `buttons`).
    pub buttons: ButtonResources<'static>,
    pub bt: BT<'static>,
    pub wifi: WIFI<'static>,
    /// UART0 — console + serial-cmd
    /// ([`crate::io::serial_cmd::SerialCmdResources`] +
    /// [`crate::io::console`]).
    pub uart0: UART0<'static>,
    /// UART0 RX pin (GPIO3).
    pub uart0_rx: AnyPin<'static>,
    /// UART0 TX pin (GPIO1).
    pub uart0_tx: AnyPin<'static>,
    /// SK6812 LED bars data pin (M-Bus pin 23 → GPIO15), driven over RMT
    /// ([`crate::driver::sk6812::Sk6812Driver`]).
    pub sk6812: AnyPin<'static>,
    /// RMT — SK6812 LED bars or the 1-Wire driver (one owner;
    /// [`crate::io::ow_temp::OnewireResources`]).
    pub rmt: RMT<'static>,
    /// PCNT — e.g. pulse counting ([`crate::io::rpm::RpmResources`]).
    pub pcnt: PCNT<'static>,
    /// External SPI PSRAM (~4 MB) — feed to `mem::init_psram_heap` (the heap
    /// region; the ESP32 cannot DMA from PSRAM). [feature `psram`]
    pub psram: esp_hal::peripherals::PSRAM<'static>,
    pub m5bus: M5Bus<'static>,
    pub system: SystemResources<'static>,
}

impl Board {
    /// Wire the Fire27 pin map. Call once with the output of
    /// [`crate::board::init`] (or `esp_hal::init`); heap setup stays with the
    /// app (sizing is application policy).
    pub fn split(peripherals: Peripherals) -> Self {
        let spi2 = Spi2Resources {
            spi2: AnySpi::from(peripherals.SPI2),
            spi2_dma: AnySpiDmaChannel::from(peripherals.DMA_SPI2),
            sck: AnyPin::from(peripherals.GPIO18),
            mosi: AnyPin::from(peripherals.GPIO23),
            miso: AnyPin::from(peripherals.GPIO19),
            display_cs: AnyPin::from(peripherals.GPIO14),
            display_dc: AnyPin::from(peripherals.GPIO27),
            display_rst: AnyPin::from(peripherals.GPIO33),
            display_bl: AnyPin::from(peripherals.GPIO32),
            card_cs: AnyPin::from(peripherals.GPIO4),
        };

        let i2c0 = I2c::new(
            peripherals.I2C0,
            I2cConfig::default()
                .with_frequency(Rate::from_khz(400))
                .with_timeout(BusTimeout::BusCycles(20)),
        )
        .expect("I2C0 init failed")
        .with_sda(AnyPin::from(peripherals.GPIO21))
        .with_scl(AnyPin::from(peripherals.GPIO22));

        let buttons = ButtonResources {
            left: AnyPin::from(peripherals.GPIO39),
            center: AnyPin::from(peripherals.GPIO38),
            right: AnyPin::from(peripherals.GPIO37),
        };

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
            buttons,
            bt: peripherals.BT,
            wifi: peripherals.WIFI,
            uart0: peripherals.UART0,
            uart0_rx: AnyPin::from(peripherals.GPIO3),
            uart0_tx: AnyPin::from(peripherals.GPIO1),
            sk6812: AnyPin::from(peripherals.GPIO15),
            rmt: peripherals.RMT,
            pcnt: peripherals.PCNT,
            psram: peripherals.PSRAM,
            m5bus: M5Bus {
                gpio5: AnyPin::from(peripherals.GPIO5),
                gpio26: AnyPin::from(peripherals.GPIO26),
            },
            system,
        }
    }
}
