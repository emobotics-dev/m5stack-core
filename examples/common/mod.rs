// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared framework for the examples in `examples/` — **not** an example itself.
//!
//! Cargo discovers `examples/*.rs` and `examples/*/main.rs` as targets. This
//! directory has no `main.rs`, so it is invisible to that discovery and can hold
//! the machinery every example needs. Each example pulls it in with one line:
//!
//! ```ignore
//! #[path = "common/mod.rs"] mod common;        // single-file example
//! #[path = "../common/mod.rs"] mod common;     // examples/<name>/main.rs
//! ```
//!
//! The split is by *role*, not by topic: [`board`] is per-board bring-up on top
//! of the BSP's `Board::split`, [`shim`] the console and heap, [`sched`] where
//! UI threads sit in the priority ladder, [`ui`] the live LVGL plumbing.
//! Anything belonging to one example — its widgets, its measurement apparatus —
//! lives beside that example instead, which is what the multi-file form is for.
//!
//! Compiled *into* each example rather than linked once, which is the price of
//! cargo-native examples with no support crate: every example sees the whole
//! framework and warns about the parts it does not use — unused items, unused
//! imports feeding them, and `boot!` itself in the `probe_*` examples that
//! deliberately do their own bring-up. The allows are that, not slack: nothing
//! here is dead except relative to one example.
#![allow(dead_code, unused_imports, unused_macros)]

pub mod board;
pub mod helpers;
pub mod net;
pub mod sched;
pub mod shim;

// BLE peer scanner, used only by the `coex` and `probe_nack` examples.
#[cfg(feature = "ex-coex")]
pub mod ble;
#[cfg(feature = "ex-lvgl")]
pub mod ui;

/// Bring the board up: peripherals, pin map, heap, scheduler, console.
///
/// Binds `$board` to the pin map with the console's peripherals already taken
/// out of it, and keeps the `Console` alive for the enclosing scope. `$profile`
/// is a [`HeapProfile`](shim::HeapProfile) variant name.
///
/// ```ignore
/// #[esp_rtos::main]
/// async fn main(spawner: Spawner) {
///     common::boot!(spawner, board, Default);
///     let (mut display, i2c) = common::board::init_display(board.spi2, board.i2c0).await;
/// ```
///
/// Which serial the console gets is the one thing that genuinely differs per
/// board (Fire27 UART0, CoreS3 USB-Serial-JTAG), so it is a `cfg` here instead
/// of eight lines repeated in every example.
///
/// The `probe_*` examples deliberately do **not** use this, and should not be
/// "tidied" into it: they need raw peripherals `Board::split` has already
/// bundled — `SPI2` as a bare master, `DMA_CH0`, the free M-Bus pads (#15) —
/// because what they measure is esp-hal *below* the BSP, where routing through
/// the BSP would contaminate the result. Being CoreS3-only, they also have no
/// per-board `cfg` to hide.
macro_rules! boot {
    ($spawner:expr, $board:ident, $profile:ident) => {
        let $board = crate::common::shim::boot_board(crate::common::shim::HeapProfile::$profile);
        esp_rtos::start($board.system.timer0_0, $board.system.sw_int.software_interrupt0);
        crate::common::boot_console!($spawner, $board);
    };
    ($spawner:expr, $board:ident, $profile:ident, idle_hook = $hook:expr) => {
        let $board = crate::common::shim::boot_board(crate::common::shim::HeapProfile::$profile);
        esp_rtos::start_with_idle_hook(
            $board.system.timer0_0,
            $board.system.sw_int.software_interrupt0,
            $hook,
        );
        crate::common::boot_console!($spawner, $board);
    };
}

/// The per-board half of [`boot!`]. Not called directly.
macro_rules! boot_console {
    ($spawner:expr, $board:ident) => {
        #[cfg(feature = "fire27")]
        let _console = crate::common::shim::init_console(
            $spawner,
            crate::common::board::console_serial($board.uart0, $board.uart0_rx, $board.uart0_tx),
        );
        #[cfg(feature = "cores3")]
        let _console = crate::common::shim::init_console(
            $spawner,
            crate::common::board::console_serial($board.usb_device),
        );
    };
}

// Path-addressable as `common::boot!` — `macro_rules!` is textually scoped
// otherwise, and each example declares this module at its own crate root.
pub(crate) use {boot, boot_console};
