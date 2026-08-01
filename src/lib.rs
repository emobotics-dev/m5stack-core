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
//!   ([`io::console::take_panic_breadcrumb`]) + (`app-desc` feature) the
//!   firmware identity, logged once as the very first BSP-emitted line.
//! - the `panic-handler` feature exports the `#[panic_handler]`, and
//!   [`app_desc!`] the esp-idf app descriptor (`app-desc` feature, implied by
//!   `heap`) — plus [`app_elf_sha256`], and, under `identity`, an enforced
//!   build-time git identity in `app_desc!()`'s version field (see the
//!   README's "Firmware identity" section).
//! - [`board::run_app_core`] — the second-core harness (`multicore` feature).
//! - [`io::input_caps`] — the board's input model (keypad vs pointer), so a UI
//!   installs the matching indev without hardcoding the board.
//!
//! Exactly one board feature must be enabled: `fire27` (xtensa-esp32) or
//! `cores3` (xtensa-esp32s3). Radio (`ble`/`wifi`/`wifi-sta`/`coex`), `heap`/
//! `psram`/`app-desc`/`identity`, `console-serial`, `panic-handler`, and
//! `multicore` are orthogonal opt-ins. See the README for the full feature
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
// crate is pulled by the `app-desc` feature (implied by `heap`); re-exported
// (hidden) so the macro can name it from the call site without the binary
// depending on it directly.
#[cfg(feature = "app-desc")]
#[doc(hidden)]
pub use esp_bootloader_esp_idf as __bootloader;

/// Reads back the descriptor [`app_desc!`] emits.
///
/// [`app_desc!`] expands to a `static` named `ESP_APP_DESC` **at its call
/// site** (i.e. in the binary, not this crate), so this reads it back by its
/// linker symbol (`esp_app_desc`, `EspAppDesc` is `#[repr(C)]`) rather than by
/// path — the one way a BSP function can reach a descriptor the *consumer*
/// created. Requires [`app_desc!`] to have been invoked somewhere in the
/// binary: otherwise this fails to **link**, not silently reads zeroes.
#[cfg(feature = "app-desc")]
fn app_desc() -> &'static __bootloader::EspAppDesc {
    unsafe extern "C" {
        #[link_name = "esp_app_desc"]
        static ESP_APP_DESC: __bootloader::EspAppDesc;
    }
    unsafe { &ESP_APP_DESC }
}

/// The ELF's content hash, straight from the esp-idf application descriptor —
/// the first bytes distinguish two images built from the same commit with
/// different uncommitted edits. Unambiguous with no consumer input needed: it
/// is a function of the linked image, computed and patched in by `espflash`.
/// Requires [`app_desc!`] to have been invoked (see [`app_desc()`]).
#[cfg(feature = "app-desc")]
pub fn app_elf_sha256() -> &'static [u8; 32] {
    app_desc().app_elf_sha256()
}

/// Logs the descriptor's version (plain `CARGO_PKG_VERSION`, or the enforced
/// `<bin>/<features>/<hash><dirty>` git mark under `identity` — which already
/// carries the binary name, so it isn't repeated separately there) and a
/// 6-byte `app_elf_sha256` prefix, once, as early as possible on boot —
/// called by [`io::console::install`], not meant to be called directly. Like
/// [`app_elf_sha256`], requires [`app_desc!`] to have been invoked somewhere
/// in the binary (a link error otherwise, not a silent no-op): any binary
/// enabling `app-desc` — directly, or via `heap` — is expected to call
/// [`app_desc!`], matching this crate's existing "thin entry shell" framing.
#[cfg(feature = "app-desc")]
pub(crate) fn log_boot_identity() {
    use core::fmt::Write as _;

    let desc = app_desc();
    let sha = desc.app_elf_sha256();
    let mut hex = heapless::String::<12>::new();
    for byte in &sha[..6] {
        let _ = write!(hex, "{byte:02x}");
    }
    #[cfg(feature = "identity")]
    log::info!("{} {} app_elf_sha256={hex}", io::console::markers::IDENTITY, desc.version());
    #[cfg(not(feature = "identity"))]
    log::info!(
        "{} {} {} app_elf_sha256={hex}",
        io::console::markers::IDENTITY,
        desc.project_name(),
        desc.version()
    );
}

/// Emit the esp-idf application descriptor. Invoke once **in the binary** (not
/// the BSP) so it captures the *application's* `CARGO_PKG_VERSION`. Thin wrapper
/// over `esp_bootloader_esp_idf::esp_app_desc!` so the binary keeps a single
/// `m5stack_core::app_desc!();` line instead of naming the bootloader crate.
///
/// With the `identity` feature off (default), the descriptor's version field
/// is plain `CARGO_PKG_VERSION`, as always. With `identity` on, this same call
/// site — unchanged — instead requires `M5STACK_CORE_BUILD_MARK`, a build-time
/// git descriptor the consumer's own `build.rs` sets (see the
/// `m5stack-core-build` crate). BSP owns the mechanism, never the content: if
/// the `build.rs` wiring is missing, `env!()` fails to compile in the
/// *consumer's* crate — a real compile error, not a silent fallback.
///
/// `project_name` is `CARGO_BIN_NAME`, not `CARGO_PKG_NAME`: a package can
/// have more than one `[[bin]]`, and only the per-binary compilation (where
/// this macro expands) knows which one is being built — `CARGO_PKG_NAME`
/// would report the same value for every binary in a multi-bin package.
#[cfg(all(feature = "app-desc", not(feature = "identity")))]
#[macro_export]
macro_rules! app_desc {
    () => {
        $crate::__bootloader::esp_app_desc!(
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_BIN_NAME"),
            $crate::__bootloader::BUILD_TIME,
            $crate::__bootloader::BUILD_DATE,
            $crate::__bootloader::ESP_IDF_COMPATIBLE_VERSION,
            $crate::__bootloader::MMU_PAGE_SIZE,
            0,
            u16::MAX,
            $crate::__bootloader::SECURE_VERSION
        );
    };
}

/// See the non-`identity` [`app_desc!`] above — same call site. The version
/// field becomes `CARGO_BIN_NAME` joined with the git mark
/// (`<bin>/<features>/<hash><dirty>`, e.g. `display/crypto-opt/0f63a4926303+`) — binary
/// name included for the same reason as `project_name` above, joined here
/// (rather than by `m5stack-core-build`) because only this per-binary
/// compilation knows `CARGO_BIN_NAME`; a `build.rs` runs once per *package*
/// and can't know which binary it's describing.
///
/// `EspAppDesc::version` is a fixed 32-byte C string with no reserved NUL
/// terminator (see `m5stack-core-build`'s docs) — 31 bytes is the true safe
/// ceiling. Enforced here as a real compile error, not a silent truncation:
/// which part to shorten (the `features` tag passed to `emit_identity_env`,
/// or nothing this crate controls) is the caller's call, not this macro's.
#[cfg(all(feature = "app-desc", feature = "identity"))]
#[macro_export]
macro_rules! app_desc {
    () => {
        const _: () = ::core::assert!(
            ::core::concat!(::core::env!("CARGO_BIN_NAME"), "/", ::core::env!("M5STACK_CORE_BUILD_MARK")).len() <= 31,
            "m5stack_core::app_desc!(): CARGO_BIN_NAME + '/' + M5STACK_CORE_BUILD_MARK exceeds \
             31 bytes (EspAppDesc::version's safe ceiling — see m5stack-core-build's docs). \
             Shorten the `features` tag passed to emit_identity_env(), or the binary name."
        );
        $crate::__bootloader::esp_app_desc!(
            ::core::concat!(::core::env!("CARGO_BIN_NAME"), "/", ::core::env!("M5STACK_CORE_BUILD_MARK")),
            env!("CARGO_BIN_NAME"),
            $crate::__bootloader::BUILD_TIME,
            $crate::__bootloader::BUILD_DATE,
            $crate::__bootloader::ESP_IDF_COMPATIBLE_VERSION,
            $crate::__bootloader::MMU_PAGE_SIZE,
            0,
            u16::MAX,
            $crate::__bootloader::SECURE_VERSION
        );
    };
}
