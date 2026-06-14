// SPDX-License-Identifier: MIT OR Apache-2.0
//! Board support crate for the **M5Stack Fire27** (ESP32) and **CoreS3**
//! (ESP32-S3).
//!
//! Provides chip-agnostic peripheral drivers ([`driver`]), a shared async I2C
//! bus and reusable `embassy`-based IO task loops ([`io`]), and board bring-up
//! helpers ([`board`]).
//!
//! The crate also owns the board/chip boilerplate a binary would otherwise
//! hand-roll, so a consumer's `main` collapses to a thin entry shell:
//!
//! - [`mem::init_heap`] — the global heap (esp-alloc DRAM regions + HIL-proven
//!   per-board sizes; `heap` feature, implied by `psram`).
//! - [`io::console::install`] — one-call logging over the chip's native
//!   transport (UART0 / USB-Serial-JTAG CDC) + an RTC panic breadcrumb
//!   ([`io::console::take_panic_breadcrumb`]).
//! - the `panic-handler` feature exports the `#[panic_handler]`, and
//!   [`app_desc!`] the esp-idf app descriptor.
//! - [`board::run_app_core`] — the second-core harness (`multicore` feature).
//! - [`io::input_caps`] — the board's input model (keypad vs pointer), so a UI
//!   installs the matching indev without hardcoding the board.
//!
//! Exactly one board feature must be enabled: `fire27` (xtensa-esp32) or
//! `cores3` (xtensa-esp32s3). Radio (`ble`/`wifi`/`wifi-sta`/`coex`), `heap`/
//! `psram`, `console-serial`, `panic-handler`, and `multicore` are orthogonal
//! opt-ins. See the README for the full feature matrix and usage examples.
#![no_std]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
// `PsramSafe` (mem::, `psram` feature) is a Send/Sync-style auto trait with
// negative impls for the atomic types. The item is `#[cfg(psram)]`, but cfg
// stripping happens *after* parsing, so the `auto trait` syntax is parsed even
// in a psram-free `heap` build and would warn unless the gate is active. The
// crate is nightly-only regardless, so enable it unconditionally.
#![feature(auto_traits, negative_impls)]
// `board::run_app_core`'s APP-core idle loop uses the Xtensa `waiti` instruction
// via inline asm, which is still unstable for this architecture.
#![cfg_attr(feature = "multicore", feature(asm_experimental_arch))]

// Link-only: pins esp-rom-sys to the version esp-hal's code actually needs
// (esp-hal 1.1.x under-constrains it to ~0.1 but calls a 0.1.4 API). Referenced
// here so the pin in Cargo.toml survives `cargo package` rather than being
// dropped as an unused dependency.
use esp_rom_sys as _;

#[macro_use]
mod fmt;

/// Replaces embassy-executor's `Spawner::must_spawn`, dropped in 0.10: panics
/// on pool exhaustion with call-site context. Works for `Spawner` and
/// `SendSpawner`.
#[macro_export]
macro_rules! must_spawn {
    ($spawner:expr, $task:expr) => {
        $spawner.spawn($task.unwrap_or_else(|e| {
            ::core::panic!(concat!("spawn ", stringify!($task), ": {:?}"), e)
        }))
    };
}

pub mod board;
pub mod driver;
pub mod io;
#[cfg(feature = "heap")]
pub mod mem;

/// BSP-provided `#[panic_handler]` (opt in with the `panic-handler` feature).
/// Body is [`io::console::on_panic`]: record the RTC breadcrumb, best-effort
/// drain the ring over the raw transport, then halt and let the RWDT recover.
/// A consumer that wants its own panic policy simply leaves the feature off.
#[cfg(feature = "panic-handler")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crate::io::console::on_panic(info)
}

// `app_desc!` wraps esp-bootloader-esp-idf's `esp_app_desc!`. The bootloader
// crate is pulled by the `heap` feature; re-exported (hidden) so the macro can
// name it from the call site without the binary depending on it directly.
#[cfg(feature = "heap")]
#[doc(hidden)]
pub use esp_bootloader_esp_idf as __bootloader;

/// Emit the esp-idf application descriptor. Invoke once **in the binary** (not
/// the BSP) so it captures the *application's* `CARGO_PKG_VERSION`. Thin wrapper
/// over `esp_bootloader_esp_idf::esp_app_desc!` so the binary keeps a single
/// `m5stack_core::app_desc!();` line instead of naming the bootloader crate.
#[cfg(feature = "heap")]
#[macro_export]
macro_rules! app_desc {
    () => {
        $crate::__bootloader::esp_app_desc!();
    };
}
