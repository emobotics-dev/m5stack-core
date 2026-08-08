// SPDX-License-Identifier: MIT OR Apache-2.0
//! Multicore harness demo: `board::run_app_core` parks + starts the APP core on
//! an esp-rtos InterruptExecutor and spawns a task there, while `main` keeps
//! running on the PRO core. Both cores log over the BSP console, so a serial
//! monitor shows interleaved `[APP core]` / `[PRO core]` ticks — proof the
//! second core is running independently (#35 C4).
//!
//! Build: `cargo +esp run --release -p demos --bin multicore --features multicore`
//! (Fire27) or add `--no-default-features --features cores3,multicore
//! --target xtensa-esp32s3-none-elf`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[path = "common/mod.rs"]
mod common;

extern crate alloc;

use embassy_executor::{SendSpawner, Spawner};
use embassy_time::{Duration, Timer};
use esp_hal::interrupt::Priority;
use esp_hal::system::Stack;
use static_cell::make_static;

m5stack_core::app_desc!();

/// Runs on the APP core (started by `run_app_core`).
#[embassy_executor::task]
async fn app_core_task() {
    let mut n = 0u32;
    loop {
        log::info!("[APP core] tick {}", n);
        n = n.wrapping_add(1);
        Timer::after(Duration::from_secs(1)).await;
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    common::boot!(spawner, board, Default);

    // Park + start the APP core on its own InterruptExecutor (SWI1) and spawn a
    // task there. The guard must outlive the program (dropping it stops the core).
    let app_stack = make_static!(Stack::<8192>::new());
    let _app_core = m5stack_core::board::run_app_core(
        board.system.cpu_ctrl,
        board.system.sw_int.software_interrupt1,
        app_stack,
        Priority::Priority3,
        |app_spawner: SendSpawner| {
            app_spawner.spawn(app_core_task().expect("spawn app_core_task"));
        },
    );

    // PRO core keeps working independently.
    let mut n = 0u32;
    loop {
        log::info!("[PRO core] tick {}", n);
        n = n.wrapping_add(1);
        Timer::after(Duration::from_secs(1)).await;
    }
}
