// SPDX-License-Identifier: MIT OR Apache-2.0
//! External PSRAM heap integration.
//!
//! Both boards carry SPI PSRAM (Fire27: ~4 MB, CoreS3: ~8 MB). `esp-alloc`
//! exposes a single global heap that can be backed by several regions;
//! [`init_psram_heap`] maps the external PSRAM and registers it as one such
//! region. After that, an application can allocate from it in two ways:
//!
//! 1. **Implicitly** — once registered, the global allocator may satisfy any
//!    `alloc::vec!` / `Box` / `String` from PSRAM (internal DRAM is consumed
//!    first, then it spills to PSRAM).
//! 2. **Explicitly** — pick the region per allocation with the marker
//!    allocators re-exported here, via the `allocator_api2` containers:
//!
//! ```ignore
//! use allocator_api2::vec::Vec;
//! use m5stack_core::mem::{ExternalMemory, InternalMemory};
//!
//! let psram_free = m5stack_core::mem::init_psram_heap(peripherals.PSRAM);
//!
//! // forced into PSRAM:
//! let mut big: Vec<u8, _> = Vec::with_capacity_in(512 * 1024, ExternalMemory);
//! // forced into internal DRAM (e.g. for DMA buffers):
//! let mut dma: Vec<u8, _> = Vec::with_capacity_in(4 * 1024, InternalMemory);
//! ```
//!
//! ## ⚠️ Caveats
//!
//! - **Atomics must not live in PSRAM.** On ESP32 / ESP32-S3 atomic
//!   instructions misbehave against PSRAM-backed memory. Keep anything holding
//!   an `Atomic*` (directly or indirectly — e.g. `Arc`, many lock types) out of
//!   PSRAM. Prefer the explicit [`ExternalMemory`] path for large plain-data
//!   buffers rather than letting arbitrary global allocations spill there.
//! - **DMA from PSRAM:** the original ESP32 (Fire27) cannot DMA directly out of
//!   PSRAM — keep SPI/I2S DMA source buffers in [`InternalMemory`]. The ESP32-S3
//!   (CoreS3) can DMA from PSRAM subject to cache/alignment rules.
//! - Build in **release** (or at least `opt-level = "s"`, which both profiles
//!   already use) — PSRAM timing calibration is unreliable at `opt-level = 0`.

pub use esp_alloc::{AnyMemory, ExternalMemory, InternalMemory};
use esp_hal::peripherals::PSRAM;

/// Map the board's external PSRAM and add it to the global heap as an
/// [`ExternalMemory`] region.
///
/// The size is auto-detected. Returns the amount of external (PSRAM) heap free
/// immediately after registration, in bytes.
///
/// Call once, after [`esp_hal::init`] and (optionally) the internal
/// [`esp_alloc::heap_allocator!`]. Calling it more than once is unsound — the
/// PSRAM controller must only be initialized a single time.
pub fn init_psram_heap(psram: PSRAM<'static>) -> usize {
    esp_alloc::psram_allocator!(psram, esp_hal::psram);
    let free = esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::External.into());
    info!("PSRAM heap registered: {} KiB external free", free / 1024);
    free
}
