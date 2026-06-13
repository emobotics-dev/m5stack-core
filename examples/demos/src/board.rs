// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-board bring-up for the demos, built on the BSP's `Board::split` +
//! `board::display` + `io` loops. The two boards' differences (display reset
//! path, input flavour, battery chip) are concentrated here so the bins read
//! identically.

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDeviceWithConfig;
use embassy_sync::mutex::Mutex;
use esp_hal::{
    Async, Blocking,
    gpio::{Level, Output, OutputConfig},
    i2c::master::I2c,
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
};
use esp_sync::RawMutex;
use lcd_async::interface::SpiInterface;
use static_cell::StaticCell;

use m5stack_core::board::display;
use m5stack_core::board::spi2::Spi2Resources;
use m5stack_core::io::shared_i2c::SharedI2cBus;

pub use m5stack_core::board::init;
pub use m5stack_core::io::buttons::{ButtonAction, ButtonEvent, ButtonId};

/// How this board takes front-panel input — for demo headings.
#[cfg(feature = "fire27")]
pub const INPUT_KIND: &str = "Buttons";
#[cfg(feature = "cores3")]
pub const INPUT_KIND: &str = "Touch";

/// The board's pin map (`Board::split(peripherals)`), selected by feature.
#[cfg(feature = "fire27")]
pub use m5stack_core::board::fire27::Board;
#[cfg(feature = "cores3")]
pub use m5stack_core::board::cores3::Board;

/// The non-DMA shared SPI bus used by the status bins (the `draw_status` loops
/// run far below the rate that needs DMA; only the LVGL bin uses the DMA path).
type SharedSpi = Mutex<RawMutex, Spi<'static, Async>>;
/// The display SPI device (plain `Output` chip-select) on the shared bus.
type DisplayDevice = SpiDeviceWithConfig<'static, RawMutex, Spi<'static, Async>, Output<'static>>;

static SHARED_SPI: StaticCell<SharedSpi> = StaticCell::new();
static I2C_BUS: StaticCell<SharedI2cBus> = StaticCell::new();

/// Bring up the internal I2C bus (`into_async()` binds its IRQ to the calling
/// core) and wrap it `'static`. Called once, by [`init_display`].
fn init_i2c_shared(i2c0: I2c<'static, Blocking>) -> &'static SharedI2cBus {
    I2C_BUS.init(SharedI2cBus::new(i2c0.into_async()))
}

fn display_spi_config() -> SpiConfig {
    SpiConfig::default()
        .with_frequency(Rate::from_khz(400))
        .with_mode(Mode::_0)
}

// --- Fire27 -----------------------------------------------------------------

#[cfg(feature = "fire27")]
pub const NAME: &str = "Fire27";

/// Fire27 display: ILI9342C with a GPIO reset pin, plain `Output` DC.
#[cfg(feature = "fire27")]
pub type Lcd = display::Ili9342c<SpiInterface<DisplayDevice, Output<'static>>, Output<'static>>;

#[cfg(feature = "fire27")]
static BACKLIGHT: StaticCell<Output<'static>> = StaticCell::new();

/// Bring up the on-board display on the non-DMA shared bus and the internal
/// I2C bus, returning both (the I2C bus is unused on Fire27's display path but
/// returned so bins that need it — m5go, i2c_scan — get the same handle).
#[cfg(feature = "fire27")]
pub async fn init_display(
    spi2: Spi2Resources<'static>,
    i2c0: I2c<'static, Blocking>,
) -> (Lcd, &'static SharedI2cBus) {
    let i2c = init_i2c_shared(i2c0);
    let cfg = display_spi_config();
    let spi = Spi::new(spi2.spi2, cfg)
        .expect("SPI2 init failed")
        .with_sck(spi2.sck)
        .with_mosi(spi2.mosi)
        .with_miso(spi2.miso)
        .into_async();
    let display_cs = Output::new(spi2.display_cs, Level::High, OutputConfig::default());
    let dc = Output::new(spi2.display_dc, Level::Low, OutputConfig::default());
    let rst = Output::new(spi2.display_rst, Level::Low, OutputConfig::default());
    let mut bl = Output::new(spi2.display_bl, Level::Low, OutputConfig::default());
    let bus = SHARED_SPI.init(Mutex::new(spi));
    let device = SpiDeviceWithConfig::new(bus, display_cs, cfg.with_frequency(Rate::from_khz(40_000)));
    let di = SpiInterface::new(device, dc);
    let display = display::init_ili9342c_with_reset(di, rst)
        .await
        .expect("display init");
    bl.set_high();
    BACKLIGHT.init(bl); // keep the backlight on for the program's lifetime
    (display, i2c)
}

/// Fire27 input: the three debounced front-panel buttons.
#[cfg(feature = "fire27")]
pub struct Input(m5stack_core::io::buttons::Buttons<'static>);

#[cfg(feature = "fire27")]
impl Input {
    pub fn new(buttons: m5stack_core::io::buttons::ButtonResources<'static>) -> Self {
        Self(buttons.into_buttons())
    }
}

/// Fire27 battery: the M5GO bottom's IP5306 fuel gauge (I2C 0x75).
#[cfg(feature = "fire27")]
pub async fn battery_line(i2c: &'static SharedI2cBus) -> heapless::String<32> {
    use core::fmt::Write;
    use m5stack_core::driver::ip5306::{IP5306_ADDR, Ip5306Driver};
    let mut s = heapless::String::new();
    let mut batt = Ip5306Driver::new(i2c, IP5306_ADDR);
    if batt.present().await {
        match (batt.battery_level().await, batt.is_charging().await) {
            (Ok(pct), Ok(chg)) => {
                let _ = write!(s, "Batt {}% {}", pct, if chg { "CHG" } else { "" });
            }
            _ => {
                let _ = write!(s, "Batt: read err");
            }
        }
    } else {
        let _ = write!(s, "IP5306 absent");
    }
    s
}

// --- CoreS3 -----------------------------------------------------------------

#[cfg(feature = "cores3")]
pub const NAME: &str = "CoreS3";

/// AXP2101 PMIC I2C address on CoreS3.
#[cfg(feature = "cores3")]
const AXP2101_ADDR: u8 = 0x34;

/// CoreS3 display: ILI9342C with no GPIO reset (AW9523B pulses it), plain
/// `Output` DC on GPIO35.
#[cfg(feature = "cores3")]
pub type Lcd = display::Ili9342c<SpiInterface<DisplayDevice, Output<'static>>>;

/// Bring up the internal I2C bus, reset + power the panel (AW9523B `LCD_RST`,
/// AXP2101 backlight + battery ADC), then the display on the non-DMA shared
/// bus. Returns the display and the `'static` I2C bus (touch, battery, ...).
#[cfg(feature = "cores3")]
pub async fn init_display(
    spi2: Spi2Resources<'static>,
    i2c0: I2c<'static, Blocking>,
) -> (Lcd, &'static SharedI2cBus) {
    use m5stack_core::driver::axp2101::Axp2101Driver;
    let i2c = init_i2c_shared(i2c0);
    // AW9523B LCD/touch reset + AXP2101 backlight (best-effort, logs internally).
    m5stack_core::board::cores3::power_display_reset(i2c).await;
    // Enable the VBAT ADC so the m5go bin can read the cell voltage.
    let mut axp = Axp2101Driver::new(i2c, AXP2101_ADDR);
    if let Err(e) = axp.enable_battery_adc().await {
        log::warn!("AXP2101 battery ADC enable failed: {:?}", e);
    }

    let cfg = display_spi_config();
    // No `.with_miso()`: this path never reads SD, so GPIO35 is a plain DC output.
    let spi = Spi::new(spi2.spi2, cfg)
        .expect("SPI2 init failed")
        .with_sck(spi2.sck)
        .with_mosi(spi2.mosi)
        .into_async();
    let display_cs = Output::new(spi2.display_cs, Level::High, OutputConfig::default());
    // DC on GPIO35: `Output::new` configures the pad's IO-MUX so the pin drives
    // (a bare register hack would leave it unrouted → black screen).
    let dc = Output::new(spi2.miso_dc, Level::Low, OutputConfig::default());
    let bus = SHARED_SPI.init(Mutex::new(spi));
    let device = SpiDeviceWithConfig::new(bus, display_cs, cfg.with_frequency(Rate::from_khz(40_000)));
    let di = SpiInterface::new(device, dc);
    let display = display::init_ili9342c(di).await.expect("display init");
    (display, i2c)
}

/// CoreS3 input: the FT6336U touch strip emulating the three front buttons.
#[cfg(feature = "cores3")]
pub struct Input(m5stack_core::io::touch_buttons::TouchButtons);

#[cfg(feature = "cores3")]
impl Input {
    pub fn new(i2c: &'static SharedI2cBus) -> Self {
        Self(m5stack_core::io::touch_buttons::TouchButtons::new(
            i2c,
            m5stack_core::io::touch_buttons::TouchButtonsConfig::default(),
        ))
    }
}

/// CoreS3 battery: the onboard AXP2101 PMIC (manages the cell, incl. the M5GO
/// bottom's).
#[cfg(feature = "cores3")]
pub async fn battery_line(i2c: &'static SharedI2cBus) -> heapless::String<32> {
    use core::fmt::Write;
    use m5stack_core::driver::axp2101::Axp2101Driver;
    let mut s = heapless::String::new();
    let mut axp = Axp2101Driver::new(i2c, AXP2101_ADDR);
    match (axp.battery_voltage_mv().await, axp.vbus_present().await) {
        (Ok(mv), Ok(vbus)) => {
            let _ = write!(s, "Batt {} mV {}", mv, if vbus { "USB" } else { "" });
        }
        _ => {
            let _ = write!(s, "Batt: read err");
        }
    }
    s
}

/// CoreS3-only: enable the M5GO bottom's 5 V LED rail via the AW9523B, with the
/// shared-VBUS contention guard (enable only when a battery is present *or* USB
/// is absent — verbatim from M5Unified). Returns whether the rail was enabled.
#[cfg(feature = "cores3")]
pub async fn enable_bus_5v(i2c: &'static SharedI2cBus) -> bool {
    use m5stack_core::driver::aw9523b::{Aw9523bDriver, Aw9523bResources};
    use m5stack_core::driver::axp2101::Axp2101Driver;
    let mut axp = Axp2101Driver::new(i2c, AXP2101_ADDR);
    let mut aw = Aw9523bDriver::new(Aw9523bResources { i2c });
    let vbus = axp.vbus_present().await.unwrap_or(true);
    let mv = axp.battery_voltage_mv().await.unwrap_or(0);
    let battery_present = mv > 3300;
    if battery_present || !vbus {
        match aw.enable_bus_5v().await {
            Ok(()) => {
                log::info!("M-Bus 5V enabled (BOOST_EN+BUS_OUT_EN); batt={}mV vbus={}", mv, vbus);
                true
            }
            Err(e) => {
                log::warn!("enable_bus_5v failed: {:?}", e);
                false
            }
        }
    } else {
        log::warn!("M-Bus 5V NOT enabled — no battery while on USB (contention guard)");
        false
    }
}

// --- Shared (both boards) ---------------------------------------------------

impl Input {
    /// Await the next unified front-panel event (button press on Fire27, touch
    /// zone on CoreS3).
    pub async fn next_event(&mut self) -> ButtonEvent {
        self.0.next_event().await
    }
}

/// Bring up the WiFi station + embassy-net stack from build-time creds. Returns
/// `None` when `ssid` is `None` (WiFi disabled) or init fails (logged). The
/// caller spawns `wifi::wifi_task(runner)` and its own net task. Identical on
/// both boards.
pub fn connect_wifi(
    wifi: esp_hal::peripherals::WIFI<'static>,
    ssid: Option<&'static str>,
    password: Option<&'static str>,
) -> Option<(
    embassy_net::Stack<'static>,
    m5stack_core::driver::radio::wifi::WifiControl,
    m5stack_core::driver::radio::wifi::WifiRunner,
)> {
    use m5stack_core::driver::radio::wifi::{self, AuthenticationMethod, IpSetup, StaCredentials};
    use static_cell::make_static;
    let ssid = ssid?;
    let rng = esp_hal::rng::Rng::new();
    let seed = ((rng.random() as u64) << 32) | rng.random() as u64;
    let resources = make_static!(embassy_net::StackResources::<3>::new());
    let creds = StaCredentials {
        ssid,
        password: password.unwrap_or(""),
        auth: AuthenticationMethod::Wpa2Personal,
    };
    match wifi::Wifi::new(wifi).and_then(|w| w.into_sta(creds, IpSetup::Dhcp, seed, resources)) {
        Ok(triple) => Some(triple),
        Err(e) => {
            log::warn!("WiFi init failed: {:?}", e);
            None
        }
    }
}

// --- LVGL display bus (DMA; lvgl bin only) ----------------------------------

#[cfg(feature = "lvgl")]
pub use lvgl_bus::lvgl_bringup;

#[cfg(feature = "lvgl")]
mod lvgl_bus {
    use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
    use m5stack_core::board::spi2::{DisplayBus, Spi2Resources};

    use super::Input;

    /// Bring up the LVGL display (DMA bus, no SD) **and** the front-panel input
    /// together, returning both. Fire27: the panel has a GPIO reset and input
    /// comes from the three buttons. CoreS3: the one I2C bus resets/powers the
    /// panel (AW9523B + AXP2101) **and** drives the FT6336U touch input — so it
    /// is created once here and shared.
    #[cfg(feature = "fire27")]
    pub async fn lvgl_bringup(
        spi2: Spi2Resources<'static>,
        buttons: m5stack_core::io::buttons::ButtonResources<'static>,
        dma_rx: DmaRxBuf,
        dma_tx: DmaTxBuf,
    ) -> (DisplayBus, Input) {
        let dbus = spi2.into_display_only(dma_rx, dma_tx).await.expect("display init");
        (dbus, Input::new(buttons))
    }

    #[cfg(feature = "cores3")]
    pub async fn lvgl_bringup(
        spi2: Spi2Resources<'static>,
        i2c0: esp_hal::i2c::master::I2c<'static, esp_hal::Blocking>,
        dma_rx: DmaRxBuf,
        dma_tx: DmaTxBuf,
    ) -> (DisplayBus, Input) {
        let i2c = super::init_i2c_shared(i2c0);
        m5stack_core::board::cores3::power_display_reset(i2c).await;
        let dbus = spi2.into_display_only(dma_rx, dma_tx).await.expect("display init");
        (dbus, Input::new(i2c))
    }
}
