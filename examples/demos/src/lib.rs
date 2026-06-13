// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared glue for the unified `m5stack-core` board demos.
//!
//! One crate, one bin per topic, board selected by the `fire27` (default) or
//! `cores3` cargo feature. The per-board bring-up that used to be duplicated in
//! two example crates now lives in the [`board`] module (built on the BSP's
//! `Board::split` + `board::display` + `io` loops); [`shim`] hides the per-board
//! logger + heap setup. Each bin keeps its own panic-handler / app-descriptor
//! top-matter inline (it is binary-crate-root policy, and a focused example
//! should show it rather than hide it behind a macro).
#![no_std]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

pub mod board;
pub mod shim;

// BLE peer scanner, used only by the `coex` bin (gated so it doesn't pull
// trouble-host into the non-coex bins).
#[cfg(feature = "coex")]
pub mod ble;
#[cfg(feature = "lvgl")]
pub mod ui;
