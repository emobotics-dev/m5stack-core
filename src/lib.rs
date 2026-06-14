// SPDX-License-Identifier: MIT OR Apache-2.0
//! Board support crate for the **M5Stack Fire27** (ESP32) and **CoreS3**
//! (ESP32-S3).
//!
//! Provides chip-agnostic peripheral drivers ([`driver`]), a shared async I2C
//! bus and reusable `embassy`-based IO task loops ([`io`]), board bring-up
//! helpers ([`board`]), and optional external-PSRAM heap integration ([`mem`],
//! behind the `psram` feature).
//!
//! Exactly one board feature must be enabled: `fire27` (xtensa-esp32) or
//! `cores3` (xtensa-esp32s3). Radio support (`ble`, `wifi`, `wifi-sta`, `coex`)
//! and `psram` are orthogonal opt-ins. See the README for the full feature
//! matrix and usage examples.
#![no_std]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
// `PsramSafe` (mem::, `psram` feature) is a Send/Sync-style auto trait with
// negative impls for the atomic types. The item is `#[cfg(psram)]`, but cfg
// stripping happens *after* parsing, so the `auto trait` syntax is parsed even
// in a psram-free `heap` build and would warn unless the gate is active. The
// crate is nightly-only regardless, so enable it unconditionally.
#![feature(auto_traits, negative_impls)]

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
