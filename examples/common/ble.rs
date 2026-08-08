// SPDX-License-Identifier: MIT OR Apache-2.0
//! BLE peer-MAC scanner for the coexistence smoke test.
//!
//! Runs the `trouble-host` central role on the BSP's `BleRadio` connector and
//! records the address of every advertiser it sees into a shared list, which the
//! main task renders next to the WiFi IP. No payload decoding — just MACs.

use core::cell::RefCell;

use embassy_futures::join::join;
use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use embassy_time::{Duration, Timer};
use heapless::Deque;
use log::{info, warn};
use m5stack_core::driver::radio::ble::BleRadio;
use trouble_host::prelude::*;

const PEERS_MAX: usize = 16;

/// Most-recently-seen advertiser addresses (raw little-endian, as reported).
static PEERS: Mutex<CriticalSectionRawMutex, RefCell<Deque<BdAddr, PEERS_MAX>>> =
    Mutex::new(RefCell::new(Deque::new()));

/// Copy the current peer list out as raw 6-byte addresses.
pub fn snapshot() -> heapless::Vec<[u8; 6], PEERS_MAX> {
    let mut out: heapless::Vec<[u8; 6], PEERS_MAX> = heapless::Vec::new();
    PEERS.lock(|cell| {
        for addr in cell.borrow().iter() {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&addr.raw()[..6]);
            let _ = out.push(mac);
        }
    });
    out
}

/// Records advertiser addresses; called from the host RX task per report batch.
struct PeerCollector;

impl EventHandler for PeerCollector {
    fn on_adv_reports(&self, mut reports: LeAdvReportsIter<'_>) {
        PEERS.lock(|cell| {
            let mut seen = cell.borrow_mut();
            while let Some(Ok(report)) = reports.next() {
                if seen.iter().any(|a| a.raw() == report.addr.raw()) {
                    continue;
                }
                if seen.is_full() {
                    seen.pop_front();
                }
                let m = report.addr.raw();
                info!(
                    "BLE discovered {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    m[5], m[4], m[3], m[2], m[1], m[0]
                );
                let _ = seen.push_back(report.addr);
            }
        });
    }
}

/// Run a passive BLE scan forever, collecting peer MACs (coexists with WiFi).
#[embassy_executor::task]
pub async fn ble_scan_task(ble: BleRadio) {
    let controller: ExternalController<_, 1> = ExternalController::new(ble.ble_connector);

    // Fixed local random address — fine for a test device.
    let address = Address::random([0xff, 0x8f, 0x1b, 0x05, 0xe4, 0xff]);
    let mut resources: HostResources<DefaultPacketPool, 1, 1> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);
    let Host {
        central,
        mut runner,
        ..
    } = stack.build();

    let mut scanner = Scanner::new(central);
    let handler = PeerCollector;

    let _ = join(runner.run_with_handler(&handler), async {
        let config = ScanConfig {
            active: false, // passive: we only want advertiser addresses
            interval: Duration::from_millis(500),
            window: Duration::from_millis(400),
            ..Default::default()
        };
        match scanner.scan(&config).await {
            Ok(_session) => loop {
                Timer::after(Duration::from_secs(2)).await;
            },
            Err(e) => warn!("BLE scan start failed: {:?}", e),
        }
    })
    .await;

    warn!("BLE scan task exited unexpectedly");
}
