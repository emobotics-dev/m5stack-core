// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared glue for the unified `m5stack-core` board demos.
//!
//! One crate, one bin per topic, board selected by the `fire27` (default) or
//! `cores3` cargo feature. The per-board bring-up that used to be duplicated in
//! two example crates now lives in the [`board`] module (built on the BSP's
//! `Board::split` + `board::display` + `io` loops); [`boot!`] hides the
//! per-board bring-up that is identical in every one. Each bin keeps its own
//! app-descriptor top-matter inline (it is binary-crate-root policy, and a
//! focused example should show it rather than hide it behind a macro).
#![no_std]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

pub mod board;
pub mod net;
pub mod shim;

// Re-exported so `boot!` can name them without the caller having to.
pub use esp_rtos;

/// Bring the board up: peripherals, pin map, heap, scheduler, console.
///
/// Binds `$board` to the pin map with the console's peripherals already taken
/// out of it, and keeps the [`Console`](m5stack_core::io::console::Console)
/// alive for the enclosing scope. `$profile` is a
/// [`HeapProfile`](shim::HeapProfile) variant name.
///
/// ```ignore
/// #[esp_rtos::main]
/// async fn main(spawner: Spawner) {
///     demos::boot!(spawner, board, Default);
///     let (mut display, i2c) = board::init_display(board.spi2, board.i2c0).await;
/// ```
///
/// Which serial the console gets is the one thing that genuinely differs per
/// board (Fire27 UART0, CoreS3 USB-Serial-JTAG), so it is a `cfg` here instead
/// of eight lines repeated in every bin.
///
/// The `*_probe` bins deliberately do **not** use this, and should not be
/// "tidied" into it: they need raw peripherals `Board::split` has already
/// bundled — `SPI2` as a bare master, `DMA_CH0`, the free M-Bus pads (#15) —
/// because what they measure is esp-hal *below* the BSP, where routing through
/// the BSP would contaminate the result. Being CoreS3-only, they also have no
/// per-board `cfg` to hide.
#[macro_export]
macro_rules! boot {
    ($spawner:expr, $board:ident, $profile:ident) => {
        let $board = $crate::shim::boot_board($crate::shim::HeapProfile::$profile);
        $crate::esp_rtos::start($board.system.timer0_0, $board.system.sw_int.software_interrupt0);
        $crate::boot_console!($spawner, $board);
    };
    ($spawner:expr, $board:ident, $profile:ident, idle_hook = $hook:expr) => {
        let $board = $crate::shim::boot_board($crate::shim::HeapProfile::$profile);
        $crate::esp_rtos::start_with_idle_hook(
            $board.system.timer0_0,
            $board.system.sw_int.software_interrupt0,
            $hook,
        );
        $crate::boot_console!($spawner, $board);
    };
}

/// The per-board half of [`boot!`]. Not called directly.
#[doc(hidden)]
#[macro_export]
macro_rules! boot_console {
    ($spawner:expr, $board:ident) => {
        #[cfg(feature = "fire27")]
        let _console = $crate::shim::init_console(
            $spawner,
            $crate::board::console_serial($board.uart0, $board.uart0_rx, $board.uart0_tx),
        );
        #[cfg(feature = "cores3")]
        let _console =
            $crate::shim::init_console($spawner, $crate::board::console_serial($board.usb_device));
    };
}

// BLE peer scanner, used only by the `coex` bin (gated so it doesn't pull
// trouble-host into the non-coex bins).
#[cfg(feature = "coex")]
pub mod ble;
#[cfg(feature = "lvgl")]
pub mod ui;
