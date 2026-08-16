// SPDX-License-Identifier: MIT OR Apache-2.0
//! Drives [`pps_loop`] against an empty bus, so the PPS path is testable here
//! rather than only under a consumer's firmware (#78).
//!
//! No module needed: with nothing at 0x35 the absent-hardware path runs end to
//! end. Expected output and its preconditions are on [`pps_loop`].
//!
//! Read-only by construction — an all-`None` setpoint issues no I2C write, so
//! this cannot command a supply that *is* fitted.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[path = "common/mod.rs"]
mod common;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use m5stack_core::io::pps::{PpsReadings, PpsResources, PpsSetpoint, pps_loop};

m5stack_core::app_desc!();

fn on_read(r: &PpsReadings) {
    log::info!(
        "PPS out={:.3} V {:.3} A  in={:.3} V  temp={:.1} C  mode={:?}  rejected={}",
        r.voltage,
        r.current,
        r.input_voltage,
        r.temperature,
        r.running_mode,
        r.rejected
    );
}

fn get_setpoint() -> PpsSetpoint {
    PpsSetpoint {
        current_limit: None,
        voltage_limit: None,
        enabled: None,
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    common::boot!(spawner, board, Default);

    let i2c = common::board::init_i2c_shared(board.i2c0);

    log::info!("probe_pps: driving pps_loop at 0x35 (no module needed)");

    // Awaited, not spawned: the return is the observation.
    pps_loop(PpsResources { i2c }, on_read, get_setpoint).await;

    log::info!("probe_pps: pps_loop returned - task stopped itself, as designed");

    // Park: a capture that just stops cannot distinguish finished from died.
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
