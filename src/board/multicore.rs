// SPDX-License-Identifier: MIT OR Apache-2.0
//! Second-core (APP CPU) bring-up harness (#35 C4).
//!
//! Every binary hand-rolls the same dance to put work on the APP core: build a
//! [`CpuControl`], defensively park the APP core (a JTAG-reset workaround),
//! create an esp-rtos [`InterruptExecutor`], start the core on a `waiti` idle
//! loop, and start the executor at a priority. [`run_app_core`] encapsulates all
//! of it; the binary supplies only the stack, the priority, and a closure that
//! spawns its APP-core tasks from the [`SendSpawner`].

extern crate alloc;

use embassy_executor::SendSpawner;
use esp_hal::interrupt::Priority;
use esp_hal::interrupt::software::SoftwareInterrupt;
use esp_hal::peripherals::CPU_CTRL;
use esp_hal::system::{AppCoreGuard, Cpu, CpuControl, Stack};
use esp_rtos::embassy::InterruptExecutor;

/// Park + start the APP core running an [`InterruptExecutor`] at `prio`; `init`
/// runs on the APP core with the executor's [`SendSpawner`] to spawn its tasks.
///
/// Encapsulates the defensive `park_core(AppCpu)` reset — probe-rs's JTAG reset
/// doesn't always return the APP-core control registers to power-on state, so a
/// JTAG-flashed boot can find the core reported "running" and `start_app_core`
/// panics with `CoreAlreadyRunning`; parking clears that, and `start_app_core`
/// unparks before the core runs. (Documented perf tradeoff on CoreS3: the
/// disturbance measurably slows LCD render — accepted for boot reliability.)
///
/// Call from the PRO core (main). The returned [`AppCoreGuard`] must be kept
/// alive for the program's lifetime — dropping it stops the APP core.
///
/// `sw_int` is one of `SystemResources::sw_int.software_interruptN`; `stack` is
/// a `&'static mut Stack<N>` the caller allocates (e.g. `mk_static!`/`StaticCell`).
pub fn run_app_core<const N: usize, const SWI: u8, F>(
    cpu_ctrl: CPU_CTRL<'static>,
    sw_int: SoftwareInterrupt<'static, SWI>,
    stack: &'static mut Stack<N>,
    prio: Priority,
    init: F,
) -> AppCoreGuard<'static>
where
    F: FnOnce(SendSpawner) + Send + 'static,
{
    let mut cpu = CpuControl::new(cpu_ctrl);
    // SAFETY: we run on the PRO core here, never the APP core being parked.
    unsafe { cpu.park_core(Cpu::AppCpu) };

    // The executor must be `'static` (its `SendSpawner` borrows it forever).
    // `InterruptExecutor<SWI>` is generic over the interrupt number, so a
    // `static`/`make_static!` (whose type can't depend on a fn generic) won't do
    // — leak a single heap allocation instead (one-time, lives for the program).
    let exec: &'static mut InterruptExecutor<SWI> =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(InterruptExecutor::new(sw_int)));

    cpu.start_app_core(stack, move || {
        let spawner = exec.start(prio);
        init(spawner);
        // The closure must never return — the executor lives on this frame.
        loop {
            // Xtensa wait-for-interrupt; the InterruptExecutor wakes on its IRQ.
            unsafe { core::arch::asm!("waiti 0") };
        }
    })
    .expect("start_app_core")
}
