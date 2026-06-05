// SPDX-License-Identifier: MIT OR Apache-2.0
#![no_std]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
// `PsramSafe` (mem::) is a Send/Sync-style auto trait with negative impls for
// the atomic types — only pulled in when the PSRAM API is enabled.
#![cfg_attr(feature = "psram", feature(auto_traits, negative_impls))]

#[macro_use]
mod fmt;

pub mod driver;
pub mod io;
#[cfg(feature = "psram")]
pub mod mem;
