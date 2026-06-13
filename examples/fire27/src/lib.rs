// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fire27 (ESP32) chip-specific bring-up shared by the example bins.
//!
//! The chip-agnostic helpers (colour wheel, splash/status rendering, I2C scan,
//! display geometry) live in the [`common`] crate; this crate holds only what
//! is specific to the Fire27 board: the concrete display type, its SPI bring-up,
//! the WiFi STA bring-up, and (under `coex`) the BLE peer scanner.
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
use lcd_async::interface::SpiInterface;
use log::info;
use m5stack_core::board::display::{Ili9342c, init_ili9342c_with_reset};
use m5stack_core::driver::radio::wifi::{
    self, AuthenticationMethod, IpSetup, StaCredentials, WifiControl, WifiRunner,
};
use m5stack_core::io::shared_i2c::SharedI2cBus;
use static_cell::make_static;

// Re-export the chip-agnostic helpers so bins can reach them via one path.
pub use common::{H, STRIP_BYTES, STRIP_H, W, draw_demo, draw_status, i2c_scan, wheel};

#[cfg(feature = "coex")]
pub mod ble;

/// The shared SPI bus, mutex-protected so the display device can borrow it for
/// `'static`. The Fire27 display is the bus's only user in these examples.
type SharedSpi = Mutex<RawMutex, Spi<'static, esp_hal::Async>>;

/// The concrete Fire27 display: an ILI9342C over SPI with a real hardware reset
/// pin (the 3rd type parameter is a GPIO `Output`, unlike CoreS3 which resets
/// the panel via its AW9523 expander).
pub type Lcd = Ili9342c<
    SpiInterface<
        SpiDeviceWithConfig<'static, RawMutex, Spi<'static, esp_hal::Async>, Output<'static>>,
        Output<'static>,
    >,
    Output<'static>,
>;

/// Bring up the Fire27 SPI display and return it together with the backlight
/// pin (returned low; the caller drives it high once init succeeds).
///
/// Encapsulates the SPI + `SpiInterface` + `Builder` sequence. `shared_spi` must
/// be a `'static` mutex cell (use `static_cell::make_static!`) so the display
/// device can borrow the bus for `'static`.
///
/// Panics if SPI2 or the panel fails to initialise — an unrecoverable peripheral
/// fault at startup that the examples cannot meaningfully continue past.
pub async fn init_display(
    shared_spi: &'static mut Option<SharedSpi>,
    spi2: SPI2<'static>,
    sck: AnyPin<'static>,
    mosi: AnyPin<'static>,
    miso: AnyPin<'static>,
    cs: AnyPin<'static>,
    dc: AnyPin<'static>,
    rst: AnyPin<'static>,
    bl: AnyPin<'static>,
) -> (Lcd, Output<'static>) {
    let spi_config = SpiConfig::default()
        .with_frequency(Rate::from_khz(400))
        .with_mode(esp_hal::spi::Mode::_0);
    let spi = Spi::new(spi2, spi_config.clone())
        .expect("SPI2 init failed")
        .with_sck(sck)
        .with_mosi(mosi)
        .with_miso(miso)
        .into_async();

    let display_cs = Output::new(cs, Level::High, OutputConfig::default());
    let bl = Output::new(bl, Level::Low, OutputConfig::default());
    let dc = Output::new(dc, Level::Low, OutputConfig::default());
    let rst = Output::new(rst, Level::Low, OutputConfig::default());

    let shared_spi: &'static mut SharedSpi = shared_spi.insert(Mutex::new(spi));
    let spi_device = SpiDeviceWithConfig::new(
        shared_spi,
        display_cs,
        spi_config.with_frequency(Rate::from_khz(40_000)).clone(),
    );
    let di = SpiInterface::new(spi_device, dc);
    let display = init_ili9342c_with_reset(di, rst)
        .await
        .expect("Display init failed");

    (display, bl)
}

/// Bring up I2C0 on the Fire27 bus pins (SDA=GPIO21, SCL=GPIO22) and wrap it in
/// a `'static` [`SharedI2cBus`] so multiple drivers can share it cooperatively.
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
            info!("WiFi init failed: {:?}", e);
            None
        }
    }
}
