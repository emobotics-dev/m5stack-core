// SPDX-License-Identifier: MIT OR Apache-2.0
//! CoreS3 (ESP32-S3) chip-specific bring-up shared by the example bins.
//!
//! The chip-agnostic helpers (colour wheel, splash/status rendering, I2C scan,
//! display geometry) live in the [`common`] crate; this crate holds only what
//! is specific to the CoreS3 board: the concrete display type and its bring-up
//! (which includes AW9523 reset pulses + AXP2101 backlight, since CoreS3 has no
//! GPIO reset pin), the WiFi STA bring-up, and (under `coex`) the BLE scanner.
#![no_std]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDeviceWithConfig;
use embassy_net::StackResources;
use embassy_sync::mutex::Mutex;
use esp_hal::{
    gpio::{AnyPin, Level, Output, OutputConfig},
    i2c::master::{BusTimeout, Config as I2cConfig, I2c},
    peripherals::{I2C0, SPI2, WIFI},
    rng::Rng,
    spi::master::{Config as SpiConfig, Spi},
    time::Rate,
};
use esp_sync::RawMutex;
use lcd_async::{
    Builder, NoResetPin,
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
};
use m5stack_core::driver::aw9523b::{Aw9523bDriver, Aw9523bResources};
use m5stack_core::driver::axp2101::Axp2101Driver;
use m5stack_core::driver::radio::wifi::{
    self, AuthenticationMethod, IpSetup, StaCredentials, WifiControl, WifiRunner,
};
use m5stack_core::io::shared_i2c::SharedI2cBus;
use rtt_target::rprintln;
use static_cell::make_static;

// Re-export the chip-agnostic helpers so bins can reach them via one path.
pub use common::{H, STRIP_BYTES, STRIP_H, W, draw_demo, draw_status, i2c_scan, wheel};

#[cfg(feature = "coex")]
pub mod ble;

/// The shared SPI bus, mutex-protected so the display device can borrow it for
/// `'static`. The CoreS3 display is the bus's only user in these examples.
type SharedSpi = Mutex<RawMutex, Spi<'static, esp_hal::Async>>;

/// The AXP2101 PMIC I2C address (CoreS3 onboard).
pub const AXP2101_ADDR: u8 = 0x34;

/// The concrete CoreS3 display: an ILI9342C over SPI with **no** GPIO reset pin
/// (the 3rd type parameter is [`NoResetPin`] — the panel is reset via the AW9523
/// expander in [`init_display`], not a dedicated GPIO).
pub type Lcd = lcd_async::Display<
    SpiInterface<
        SpiDeviceWithConfig<'static, RawMutex, Spi<'static, esp_hal::Async>, Output<'static>>,
        Output<'static>,
    >,
    ILI9342CRgb565,
    NoResetPin,
>;

/// Bring up I2C0 on the CoreS3 bus pins (SDA=GPIO12, SCL=GPIO11) and wrap it in
/// a `'static` [`SharedI2cBus`] so the PMIC, expander, touch and display reset
/// can share it cooperatively.
///
/// Panics if I2C0 fails to initialise (unrecoverable peripheral fault at start).
pub fn init_i2c(
    i2c0: I2C0<'static>,
    sda: AnyPin<'static>,
    scl: AnyPin<'static>,
) -> &'static SharedI2cBus {
    let i2c = I2c::new(
        i2c0,
        I2cConfig::default()
            .with_frequency(Rate::from_khz(400))
            .with_timeout(BusTimeout::BusCycles(20)),
    )
    .expect("I2C0 init failed")
    .with_sda(sda)
    .with_scl(scl)
    .into_async();
    make_static!(SharedI2cBus::new(i2c))
}

/// Bring up the CoreS3 SPI display.
///
/// Unlike Fire27 there is no GPIO reset pin: the AW9523 expander pulses the LCD
/// and touch resets, and the AXP2101 DLDO1 rail powers the backlight (both done
/// here over the shared I2C bus). Also enables the AXP2101 VBAT ADC so the bins
/// can read battery voltage. Returns the initialised display and the AXP2101
/// driver (kept by the caller for battery / 5V-rail control).
///
/// Panics if SPI2 or the panel fails to initialise — an unrecoverable
/// peripheral fault at startup the examples cannot continue past.
pub async fn init_display(
    i2c_bus: &'static SharedI2cBus,
    shared_spi: &'static mut Option<SharedSpi>,
    spi2: SPI2<'static>,
    sck: AnyPin<'static>,
    mosi: AnyPin<'static>,
    cs: AnyPin<'static>,
    dc: AnyPin<'static>,
) -> (Lcd, Axp2101Driver) {
    // --- AW9523B: LCD + touch reset ---
    let mut aw = Aw9523bDriver::new(Aw9523bResources { i2c: i2c_bus });
    if let Err(e) = aw.init().await {
        rprintln!("AW9523B init failed: {:?}", e);
    }
    if let Err(e) = aw.lcd_rst_pulse().await {
        rprintln!("AW9523B LCD RST failed: {:?}", e);
    }
    if let Err(e) = aw.touch_rst_pulse().await {
        rprintln!("AW9523B TOUCH RST failed: {:?}", e);
    }

    // --- AXP2101: backlight + battery ADC ---
    let mut axp = Axp2101Driver::new(i2c_bus, AXP2101_ADDR);
    if let Err(e) = axp.set_dldo1(true, 3300).await {
        rprintln!("AXP2101 backlight enable failed: {:?}", e);
    }
    // CoreS3 manages the battery (incl. the M5GO bottom's cell) via this AXP2101,
    // not the bottom's IP5306. Enable the VBAT ADC so we can read the voltage.
    if let Err(e) = axp.enable_battery_adc().await {
        rprintln!("AXP2101 battery ADC enable failed: {:?}", e);
    }

    // --- SPI display (GPIO35 = DC, no RST pin — handled by AW9523B) ---
    let spi_config = SpiConfig::default()
        .with_frequency(Rate::from_khz(400))
        .with_mode(esp_hal::spi::Mode::_0);
    let spi = Spi::new(spi2, spi_config.clone())
        .expect("SPI2 init failed")
        .with_sck(sck)
        .with_mosi(mosi)
        .into_async();

    let display_cs = Output::new(cs, Level::Low, OutputConfig::default());

    let shared_spi: &'static mut SharedSpi = shared_spi.insert(Mutex::new(spi));
    let spi_device = SpiDeviceWithConfig::new(
        shared_spi,
        display_cs,
        spi_config.with_frequency(Rate::from_khz(40_000)).clone(),
    );
    // Display DC on GPIO35. The example doesn't use SD/MISO, so GPIO35 is a
    // plain output here — `Output::new` configures the pad's IO-MUX so the pin
    // actually drives (a bare GPIO-register hack leaves the pad unrouted and DC
    // never toggles, so the panel never wakes → black screen).
    let dc = Output::new(dc, Level::Low, OutputConfig::default());
    let di = SpiInterface::new(spi_device, dc);
    let mut delay = embassy_time::Delay;
    let display = Builder::new(ILI9342CRgb565, di)
        .invert_colors(ColorInversion::Inverted)
        .color_order(ColorOrder::Bgr)
        .display_size(320, 240)
        .init(&mut delay)
        .await
        .expect("Display init failed");

    (display, axp)
}

/// Bring up the WiFi station and the embassy-net stack from build-time creds.
///
/// The app supplies a seed (here from the RNG; real apps derive it from their
/// TRNG) and the `StackResources`; this wraps the seed/creds/`into_sta` block.
/// Returns `None` when `ssid` is `None` (WiFi disabled) or init fails (logged).
/// The caller spawns `wifi::wifi_task(runner)` and its own net task.
pub fn connect_wifi(
    wifi: WIFI<'static>,
    ssid: Option<&'static str>,
    password: Option<&'static str>,
) -> Option<(embassy_net::Stack<'static>, WifiControl, WifiRunner)> {
    let ssid = ssid?;
    let rng = Rng::new();
    let seed = ((rng.random() as u64) << 32) | rng.random() as u64;
    let resources = make_static!(StackResources::<3>::new());
    let creds = StaCredentials {
        ssid,
        password: password.unwrap_or(""),
        auth: AuthenticationMethod::Wpa2Personal,
    };
    match wifi::Wifi::new(wifi).and_then(|w| w.into_sta(creds, IpSetup::Dhcp, seed, resources)) {
        Ok(triple) => Some(triple),
        Err(e) => {
            rprintln!("WiFi init failed: {:?}", e);
            None
        }
    }
}
