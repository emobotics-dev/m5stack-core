// SPDX-License-Identifier: MIT OR Apache-2.0
//! #10 REAL-mechanism repro: I²C-completion-IRQ ↔ RWBLE same-core contention.
//!
//! Per `alternator-regulator/docs/irq-core-binding.md`, the fire27 #10 wedge is
//! NOT the clock reset — it's the **I²C completion IRQ** binding to the **PRO
//! core** (where `into_async()` runs, in `main`), sharing core + interrupt
//! level 1 with **RWBLE** (the radio controller IRQ). Sustained ~50 Hz I²C
//! completion-IRQ traffic delays RWBLE past the BLE blob's timing assumptions →
//! desync → the **PRO-core thread-mode executor silently wedges** in ~5 s. Wedge
//! scales with I²C transaction RATE (1 Hz ~0%, 2 Hz ~10%, 10 Hz ~100%).
//!
//! My earlier harness never reproduced it because it used **blocking**
//! `write_read` (no completion IRQ) and chased the (irrelevant) reset storm.
//! This one fixes both: **async** `write_read_async` to absent `0x35` at ~100 Hz,
//! I²C constructed in `main` (PRO → IRQ on PRO), BLE scanner active (RWBLE on
//! PRO). The heartbeat + flood run on the **PRO thread-mode executor** — the
//! wedge victim — so if beats STOP while boot succeeded, the wedge reproduced.
//!
//! Build `--features coex` (BLE link). `--release`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[path = "common/mod.rs"]
mod common;

use core::sync::atomic::{AtomicU32, Ordering};

use crate::common::board;
use crate::common::{ble, net};
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::system::Cpu;
use m5stack_core::driver::radio::ble::BleRadio;
use m5stack_core::driver::radio::wifi;
use m5stack_core::io::shared_i2c::SharedI2cBus;

m5stack_core::app_desc!();

/// WiFi creds (build-time env) — the original #10 ran WiFi+BLE coex, so enable
/// both for the faithful contention context.
const WIFI_SSID: Option<&str> = option_env!("WIFI_SSID");
const WIFI_PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

/// Absent PPS address — every async transaction NACKs at the address phase but
/// still completes via the I²C completion IRQ (the contention source).
const NACK_ADDR: u8 = 0x35;
/// ~100 Hz async transactions — well above the doc's 10 Hz→100%-wedge threshold.
const POLL_INTERVAL_MS: u64 = 10;
static NACK_COUNT: AtomicU32 = AtomicU32::new(0);

/// Flood the absent address with **async** transactions. Each completes via the
/// I²C completion IRQ on the core that owns it (PRO — `into_async` ran in `main`),
/// piling level-1 IRQ traffic onto RWBLE's core.
#[embassy_executor::task]
async fn nack_flood(i2c: &'static SharedI2cBus) {
    loop {
        let mut buf = [0u8; 1];
        let r = { i2c.lock().await.write_read_async(NACK_ADDR, &[0u8], &mut buf).await };
        let n = NACK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 200 == 0 {
            log::warn!("nack_flood: {} async NACKs to 0x{:02x} (last err = {:?})", n, NACK_ADDR, r.err());
        }
        Timer::after(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

/// PRO thread-mode liveness beat — the wedge victim. Stops if RWBLE desyncs.
#[embassy_executor::task]
async fn heartbeat() {
    let mut beat: u32 = 0;
    loop {
        beat = beat.wrapping_add(1);
        log::info!(
            "ALIVE beat={} t={}ms nacks={} core={:?}",
            beat, Instant::now().as_millis(), NACK_COUNT.load(Ordering::Relaxed), Cpu::current()
        );
        Timer::after(Duration::from_millis(1000)).await;
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    common::boot!(spawner, board, Coex);
    log::info!("nack_repro-irq: main (PRO thread-mode) core = {:?}", Cpu::current());

    // --- WiFi STA + net activity: full coex load (the original #10 context) ---
    match board::connect_wifi(board.wifi, WIFI_SSID, WIFI_PASSWORD) {
        Some((stack, control, runner)) => {
            spawner.spawn(wifi::wifi_task(runner).unwrap());
            spawner.spawn(net::net_demo(stack, control).unwrap());
            log::info!("nack_repro-irq: WiFi STA + net started (coex)");
        }
        None => log::warn!("nack_repro-irq: WiFi disabled (no creds) — BLE-only"),
    }

    // --- BLE scanner: RWBLE on PRO (radio controller IRQ, level 1) ---
    match BleRadio::new(board.bt) {
        Ok(radio) => {
            spawner.spawn(ble::ble_scan_task(radio).unwrap());
            log::info!("nack_repro-irq: BLE scanner started (RWBLE on PRO)");
        }
        Err(e) => log::warn!("nack_repro-irq: BLE init FAILED ({:?}) — repro invalid w/o radio", e),
    }

    // --- I²C: into_async() runs HERE on PRO → completion IRQ binds PRO (same
    //     core + level 1 as RWBLE). This is the #10 mis-binding, on purpose. ---
    let i2c = board::init_i2c_shared(board.i2c0);
    spawner.spawn(nack_flood(i2c).expect("spawn nack_flood"));
    spawner.spawn(heartbeat().expect("spawn heartbeat"));
    log::info!(
        "nack_repro-irq: async I²C flood on 0x{:02x} @ {} ms — I²C IRQ on PRO (contends RWBLE)",
        NACK_ADDR, POLL_INTERVAL_MS
    );
}
