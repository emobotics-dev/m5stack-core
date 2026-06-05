// SPDX-License-Identifier: MIT OR Apache-2.0
//! WiFi sub-function of the shared radio.
//!
//! Layered, fully-async surface built on `esp-radio`:
//!
//! - **`wifi` feature** — [`Wifi`]: owns the `WifiController` and exposes
//!   on-demand scanning ([`Wifi::scan`]). No IP stack, no `embassy-net`.
//! - **`wifi-sta` feature** — [`Wifi::into_sta`]: consumes the handle and returns
//!   a DHCP/static `embassy_net::Stack`, a [`WifiControl`] command handle, and a
//!   [`WifiRunner`] you spawn via [`wifi_task`]. The runner owns the single
//!   controller and manages association (auto-connect + reconnect), so scanning
//!   while associated is routed through [`WifiControl::scan`] — there is never
//!   more than one owner of the controller.
//!
//! ## Mode roadmap
//!
//! Only station (STA) is wired today. The actor in [`wifi_task`] is the single
//! controller owner, so AP mode (`wifi-ap`) slots in as an `into_ap` constructor
//! plus a `Config::AccessPoint`; ESP-NOW / sniffer / CSI are separate esp-radio
//! features layered the same way.
//!
//! ## Variant differences (Fire27 / ESP32 vs CoreS3 / ESP32-S3)
//!
//! The esp-radio WiFi API is identical on both chips; the differences are RAM:
//! - The RX/TX buffer counts in [`controller_config`] are trimmed on Fire27
//!   (ESP32) where internal SRAM is tighter, and left at the esp-radio defaults
//!   on CoreS3 (ESP32-S3).
//! - **Fire27 cannot DMA out of PSRAM** — the application must keep the
//!   `embassy_net::StackResources` and socket buffers in internal RAM. CoreS3 has
//!   headroom and may place large app buffers in PSRAM (see [`crate::mem`]).
//! - With BLE coexistence (`coex`), budget the extra controller heap (~96 KB
//!   reclaimed on ESP32, less on S3).

use esp_hal::peripherals::WIFI;
use esp_radio::wifi::{ControllerConfig, Interfaces, WifiController, scan::ScanConfig};
use thiserror_no_std::Error;

pub use esp_radio::wifi::{AuthenticationMethod, ap::AccessPointInfo};

/// Scan results, capped at 16 access points.
pub type ScanList = heapless::Vec<AccessPointInfo, 16>;

/// Errors from the WiFi sub-system.
#[derive(Debug, Error)]
pub enum WifiError {
    /// Error reported by the underlying esp-radio WiFi controller.
    #[error("WiFi controller error")]
    Controller(#[from] esp_radio::wifi::WifiError),
}

/// Low-level WiFi handle: owns the controller and its interfaces.
///
/// Create with [`Wifi::new`] for scan-only use, or call [`Wifi::into_sta`]
/// (requires `wifi-sta`) to bring up a station with an `embassy_net::Stack`.
pub struct Wifi {
    controller: WifiController<'static>,
    // Only consumed by `into_sta` (the `wifi-sta` feature); scan-only builds hold
    // it for the station device but never read it.
    #[cfg_attr(not(feature = "wifi-sta"), allow(dead_code))]
    interfaces: Interfaces<'static>,
}

impl Wifi {
    /// Create and start the WiFi controller with a chip-tuned configuration.
    ///
    /// `esp_rtos::start(..)` must already be running.
    pub fn new(wifi: WIFI<'static>) -> Result<Self, WifiError> {
        let (controller, interfaces) = esp_radio::wifi::new(wifi, controller_config())?;
        Ok(Self {
            controller,
            interfaces,
        })
    }

    /// Run a default scan and return up to [`ScanList`]'s capacity of results.
    pub async fn scan(&mut self) -> Result<ScanList, WifiError> {
        self.scan_with(&ScanConfig::default()).await
    }

    /// Run a scan with an explicit [`ScanConfig`].
    pub async fn scan_with(&mut self, config: &ScanConfig) -> Result<ScanList, WifiError> {
        Ok(to_scan_list(self.controller.scan_async(config).await?))
    }
}

/// Build the `ControllerConfig`, trimming buffers per chip.
///
/// Defaults are tuned for CoreS3 (ESP32-S3). Fire27 (ESP32) trims the dynamic RX
/// buffer count to save internal SRAM — it stays well above `rx_ba_win` (6) so
/// the controller's validation is satisfied.
fn controller_config() -> ControllerConfig {
    let config = ControllerConfig::default();
    #[cfg(feature = "fire27")]
    let config = config.with_dynamic_rx_buf_num(16);
    config
}

/// Truncate the controller's scan `Vec` into a fixed-capacity [`ScanList`].
fn to_scan_list(aps: impl IntoIterator<Item = AccessPointInfo>) -> ScanList {
    let mut out = ScanList::new();
    for ap in aps.into_iter().take(out.capacity()) {
        // capacity-bounded by take(): push cannot fail.
        let _ = out.push(ap);
    }
    out
}

#[cfg(feature = "wifi-sta")]
pub use sta::*;

#[cfg(feature = "wifi-sta")]
mod sta {
    use super::*;
    use embassy_futures::{
        join::join,
        select::{Either, select},
    };
    use embassy_sync::{
        blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex, signal::Signal,
    };
    use embassy_time::{Duration, Timer};
    use esp_radio::wifi::{Config, Interface, PowerSaveMode, sta::StationConfig};

    /// Station credentials for [`Wifi::into_sta`].
    pub struct StaCredentials<'a> {
        /// Network SSID.
        pub ssid: &'a str,
        /// Pre-shared key (empty for open networks).
        pub password: &'a str,
        /// Authentication method (`Wpa2Personal` is a sane default).
        pub auth: AuthenticationMethod,
    }

    /// How the station obtains its IPv4 configuration.
    pub enum IpSetup {
        /// Acquire an address via DHCP.
        Dhcp,
        /// Use a fixed address/gateway/DNS.
        Static(embassy_net::StaticConfigV4),
    }

    enum Command {
        Scan(ScanConfig),
        Connect,
        Disconnect,
    }

    // Carried through a single static `Signal`, which reserves the max size
    // regardless; boxing the scan result would only add an allocation.
    #[allow(clippy::large_enum_variant)]
    enum Response {
        Scan(Result<ScanList, WifiError>),
        Unit(Result<(), WifiError>),
    }

    // The controller is a single, non-shareable resource owned by `wifi_task`.
    // `WifiControl` talks to it over these signals; `CTRL_LOCK` keeps at most one
    // command outstanding so a reply is never clobbered before it is read.
    static CMD: Signal<CriticalSectionRawMutex, Command> = Signal::new();
    static RESP: Signal<CriticalSectionRawMutex, Response> = Signal::new();
    static CTRL_LOCK: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());

    /// Cloneable-free command handle for the running station.
    ///
    /// Association is managed automatically by [`wifi_task`]; use this to scan
    /// while associated, or to explicitly stop/start association.
    pub struct WifiControl {
        _private: (),
    }

    /// The owned controller + network runner. Hand to [`wifi_task`].
    pub struct WifiRunner {
        controller: WifiController<'static>,
        net: embassy_net::Runner<'static, Interface<'static>>,
        desired_up: bool,
    }

    impl Wifi {
        /// Configure station mode and build the `embassy_net` stack.
        ///
        /// `seed` should come from the application's TRNG (the BSP deliberately
        /// does not claim `RNG`). `resources` is typically a
        /// `static_cell`-allocated `StackResources<N>`. On Fire27 keep it (and
        /// socket buffers) in internal RAM — that chip cannot DMA from PSRAM.
        ///
        /// Returns the `Stack` (await `stack.wait_config_up()` for an IP), the
        /// [`WifiControl`] handle, and the [`WifiRunner`] to spawn.
        pub fn into_sta<const N: usize>(
            self,
            creds: StaCredentials<'_>,
            ip: IpSetup,
            seed: u64,
            resources: &'static mut embassy_net::StackResources<N>,
        ) -> Result<(embassy_net::Stack<'static>, WifiControl, WifiRunner), WifiError> {
            let Wifi {
                mut controller,
                interfaces,
            } = self;

            let station = StationConfig::default()
                .with_ssid(creds.ssid)
                .with_password(creds.password.into())
                .with_auth_method(creds.auth);
            controller.set_config(&Config::Station(station))?;
            // Favor responsiveness over the blob's default power-save heuristics.
            controller.set_power_saving(PowerSaveMode::None)?;

            let net_config = match ip {
                IpSetup::Dhcp => embassy_net::Config::dhcpv4(Default::default()),
                IpSetup::Static(cfg) => embassy_net::Config::ipv4_static(cfg),
            };
            let (stack, net) = embassy_net::new(interfaces.station, net_config, resources, seed);

            Ok((
                stack,
                WifiControl { _private: () },
                WifiRunner {
                    controller,
                    net,
                    desired_up: true,
                },
            ))
        }
    }

    impl WifiControl {
        /// Scan with default settings while the station keeps running.
        pub async fn scan(&self) -> Result<ScanList, WifiError> {
            self.scan_with(ScanConfig::default()).await
        }

        /// Scan with an explicit [`ScanConfig`].
        pub async fn scan_with(&self, config: ScanConfig) -> Result<ScanList, WifiError> {
            let _guard = CTRL_LOCK.lock().await;
            CMD.signal(Command::Scan(config));
            match RESP.wait().await {
                Response::Scan(result) => result,
                Response::Unit(_) => unreachable!("scan command yields a scan response"),
            }
        }

        /// Request (re)association to the configured AP.
        pub async fn connect(&self) -> Result<(), WifiError> {
            self.unit_command(Command::Connect).await
        }

        /// Disconnect and stop auto-reconnect until [`WifiControl::connect`].
        pub async fn disconnect(&self) -> Result<(), WifiError> {
            self.unit_command(Command::Disconnect).await
        }

        async fn unit_command(&self, command: Command) -> Result<(), WifiError> {
            let _guard = CTRL_LOCK.lock().await;
            CMD.signal(command);
            match RESP.wait().await {
                Response::Unit(result) => result,
                Response::Scan(_) => unreachable!("unit command yields a unit response"),
            }
        }
    }

    /// Drive the network stack and manage station association.
    ///
    /// Joins the `embassy_net` runner with the controller actor; neither future
    /// returns, so this task runs for the lifetime of the program.
    #[embassy_executor::task]
    pub async fn wifi_task(runner: WifiRunner) {
        let WifiRunner {
            mut controller,
            mut net,
            mut desired_up,
        } = runner;
        join(net.run(), run_actor(&mut controller, &mut desired_up)).await;
    }

    /// Single owner of the controller: auto-connects, reconnects on link loss,
    /// and services [`WifiControl`] commands. Races association progress against
    /// incoming commands so a scan/disconnect is handled promptly.
    async fn run_actor(controller: &mut WifiController<'static>, desired_up: &mut bool) {
        loop {
            if *desired_up && !controller.is_connected() {
                let outcome = {
                    let connect = controller.connect_async();
                    select(connect, CMD.wait()).await
                };
                match outcome {
                    Either::First(Ok(_)) => {} // connected — fall through to the wait branch
                    Either::First(Err(error)) => {
                        warn!("WiFi connect failed: {:?}", error);
                        // Back off, but still service commands during the wait.
                        let backoff = select(Timer::after(Duration::from_secs(2)), CMD.wait()).await;
                        if let Either::Second(command) = backoff {
                            handle(controller, desired_up, command).await;
                        }
                    }
                    Either::Second(command) => handle(controller, desired_up, command).await,
                }
            } else if *desired_up {
                let outcome = {
                    let disconnect = controller.wait_for_disconnect_async();
                    select(disconnect, CMD.wait()).await
                };
                if let Either::Second(command) = outcome {
                    handle(controller, desired_up, command).await;
                }
                // Either::First => link lost; loop reconnects.
            } else {
                let command = CMD.wait().await;
                handle(controller, desired_up, command).await;
            }
        }
    }

    async fn handle(
        controller: &mut WifiController<'static>,
        desired_up: &mut bool,
        command: Command,
    ) {
        match command {
            Command::Scan(config) => {
                let result = controller
                    .scan_async(&config)
                    .await
                    .map(to_scan_list)
                    .map_err(WifiError::from);
                RESP.signal(Response::Scan(result));
            }
            Command::Connect => {
                *desired_up = true;
                RESP.signal(Response::Unit(Ok(())));
            }
            Command::Disconnect => {
                *desired_up = false;
                let result = controller
                    .disconnect_async()
                    .await
                    .map(|_| ())
                    .map_err(WifiError::from);
                RESP.signal(Response::Unit(result));
            }
        }
    }
}
