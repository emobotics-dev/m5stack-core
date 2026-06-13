// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared WiFi STA helper: the `net_demo` task (DHCP wait + AP scan) plus a
//! snapshot of the last scan so the `wifi_sta` / `coex` bins can show the
//! nearby networks on screen, not just in the log.

use core::cell::RefCell;
use core::fmt::Write;

use embassy_net::Stack;
use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use m5stack_core::driver::radio::wifi::WifiControl;

/// Max APs surfaced on screen (the log shows all of them).
const MAX_APS: usize = 6;
type ScanList = heapless::Vec<heapless::String<32>, MAX_APS>;

static SCAN: Mutex<CriticalSectionRawMutex, RefCell<ScanList>> =
    Mutex::new(RefCell::new(heapless::Vec::new()));

/// The most recent AP scan, each entry pre-formatted `SSID  -NN` for the panel.
pub fn scan_lines() -> ScanList {
    SCAN.lock(|c| c.borrow().clone())
}

/// Wait for a DHCP lease, log the IP, scan for APs (logging all of them), and
/// stash the first [`MAX_APS`] for the on-screen panel. Spawned by the
/// `wifi_sta` / `coex` bins.
#[embassy_executor::task]
pub async fn net_demo(stack: Stack<'static>, control: WifiControl) {
    log::info!("WiFi: connecting + waiting for DHCP...");
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        log::info!("WiFi: got IP {}", cfg.address);
    }
    match control.scan().await {
        Ok(aps) => {
            log::info!("WiFi scan: {} AP(s)", aps.len());
            for ap in aps.iter() {
                log::info!(
                    "  {:<32} ch{:>2} {:>4} dBm",
                    ap.ssid.as_str(),
                    ap.channel,
                    ap.signal_strength
                );
            }
            // Tight critical section: format + store, no logging/IO inside.
            SCAN.lock(|c| {
                let mut v = c.borrow_mut();
                v.clear();
                for ap in aps.iter().take(MAX_APS) {
                    let mut line = heapless::String::new();
                    let _ = write!(line, "{:<20.20} {:>4}", ap.ssid.as_str(), ap.signal_strength);
                    let _ = v.push(line);
                }
            });
        }
        Err(e) => log::info!("WiFi scan failed: {:?}", e),
    }
}
