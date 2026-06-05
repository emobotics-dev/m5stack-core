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
//! 2. **Explicitly** — pick the region per allocation. Prefer the *checked*
//!    helpers [`psram_box`] / [`psram_vec`], which reject atomic-bearing types
//!    at compile time (see [`PsramSafe`]):
//!
//! ```ignore
//! use m5stack_core::mem;
//!
//! let psram_free = mem::init_psram_heap(peripherals.PSRAM);
//!
//! let mut big = mem::psram_vec::<u8>(512 * 1024);   // in PSRAM, atomics rejected
//! let scratch = mem::psram_box([0u32; 1024]);       // in PSRAM
//! let dma = mem::dma_buffer(4 * 1024);              // in internal DRAM, DMA-safe
//! ```
//!
//! The raw marker allocators ([`ExternalMemory`] / [`InternalMemory`]) are also
//! re-exported as an escape hatch for `allocator_api2` containers, but they do
//! **not** perform the atomic check — reach for them only when you know what
//! you are placing in PSRAM.
//!
//! ## Enforced vs. documented caveats
//!
//! - **Atomics must not live in PSRAM.** *Enforced* on the checked path:
//!   [`psram_box`] / [`psram_vec`] bound `T: PsramSafe`, so anything holding an
//!   `Atomic*` (directly or transitively) fails to compile.
//! - **DMA from PSRAM:** the original ESP32 (Fire27) cannot DMA out of PSRAM.
//!   *Guarded* by [`assert_dma_capable`] (a `debug_assert` on Fire27, a no-op on
//!   CoreS3, which can DMA from PSRAM); use [`dma_buffer`] to get an
//!   internal-DRAM buffer in the first place.
//! - **opt-level > 0:** *Enforced* at build time — enabling the `psram` feature
//!   with `opt-level = 0` fails the build (see `build.rs`). PSRAM timing
//!   calibration is unreliable unoptimized.

use core::sync::atomic::{
    AtomicBool, AtomicI8, AtomicI16, AtomicI32, AtomicIsize, AtomicPtr, AtomicU8, AtomicU16,
    AtomicU32, AtomicUsize,
};

use allocator_api2::{boxed::Box, vec::Vec};
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

/// Marker for types safe to store in PSRAM: nothing holding an *inline* atomic.
///
/// Atomic read-modify-write instructions misbehave against PSRAM-backed
/// addresses on ESP32 / ESP32-S3, so the checked allocators [`psram_box`] /
/// [`psram_vec`] only accept `T: PsramSafe`. Like `Send` / `Sync` this is an
/// auto trait: a struct is `PsramSafe` iff every field is, so a type that
/// embeds an `Atomic*` (directly or transitively — e.g. via `Arc`, many lock
/// types) is rejected at compile time.
///
/// A *pointer or reference* to an atomic living elsewhere is fine — the atomic
/// itself is not in PSRAM — so `&T`, `&mut T`, `*const T` and `*mut T` are
/// always `PsramSafe`.
///
/// # Safety
/// Only implement (or negative-impl) this to reflect the atomic-in-PSRAM
/// hazard; the checked allocators rely on it to keep atomics out of PSRAM.
pub unsafe auto trait PsramSafe {}

impl !PsramSafe for AtomicBool {}
impl !PsramSafe for AtomicI8 {}
impl !PsramSafe for AtomicU8 {}
impl !PsramSafe for AtomicI16 {}
impl !PsramSafe for AtomicU16 {}
impl !PsramSafe for AtomicI32 {}
impl !PsramSafe for AtomicU32 {}
impl !PsramSafe for AtomicIsize {}
impl !PsramSafe for AtomicUsize {}
impl<T> !PsramSafe for AtomicPtr<T> {}

// A pointer/reference to an atomic is fine — the atomic lives elsewhere.
// (Mirrors `unsafe impl<T: ?Sized> Send for &T` in std.)
unsafe impl<T: ?Sized> PsramSafe for &T {}
unsafe impl<T: ?Sized> PsramSafe for &mut T {}
unsafe impl<T: ?Sized> PsramSafe for *const T {}
unsafe impl<T: ?Sized> PsramSafe for *mut T {}

/// Allocate `value` in external PSRAM. Atomic-bearing `T` is rejected at
/// compile time via [`PsramSafe`].
pub fn psram_box<T: PsramSafe>(value: T) -> Box<T, ExternalMemory> {
    Box::new_in(value, ExternalMemory)
}

/// A `Vec<T>` with room for `capacity` elements reserved in external PSRAM.
/// Atomic-bearing `T` is rejected at compile time via [`PsramSafe`].
pub fn psram_vec<T: PsramSafe>(capacity: usize) -> Vec<T, ExternalMemory> {
    Vec::with_capacity_in(capacity, ExternalMemory)
}

/// A zeroed byte buffer in internal DRAM, suitable as a DMA buffer.
///
/// Convenience for the common "I need a DMA-capable scratch buffer" case so the
/// allocator does not have to be spelled out. The result is DMA-reachable on
/// both chips; pair it with [`assert_dma_capable`] if a buffer's origin is ever
/// in doubt.
pub fn dma_buffer(len: usize) -> Vec<u8, InternalMemory> {
    let mut v = Vec::with_capacity_in(len, InternalMemory);
    v.resize(len, 0);
    v
}

/// Debug-assert that `buf` is DMA-reachable on this chip.
///
/// On the ESP32 (Fire27) the DMA engine cannot reach the PSRAM-mapped data
/// window, so a PSRAM-backed buffer handed to SPI/I2S DMA silently corrupts.
/// This catches that on first use under `debug_assertions`. It is a no-op
/// (compiled away) on the ESP32-S3, which *can* DMA from PSRAM.
#[cfg(feature = "fire27")]
#[inline]
pub fn assert_dma_capable(buf: &[u8]) {
    // ESP32 external RAM (PSRAM) is cache-mapped into this data window; internal
    // DRAM lives above it. Constant per the ESP32 TRM external-memory map.
    const PSRAM_DATA_WINDOW: core::ops::Range<usize> = 0x3F80_0000..0x3FC0_0000;
    let p = buf.as_ptr() as usize;
    debug_assert!(
        !PSRAM_DATA_WINDOW.contains(&p),
        "DMA buffer at {p:#x} lives in PSRAM; the ESP32 cannot DMA to/from PSRAM \
         — allocate it in InternalMemory (see mem::dma_buffer)"
    );
}

/// No-op on every target except the ESP32 (Fire27); see the Fire27 variant.
#[cfg(not(feature = "fire27"))]
#[inline]
pub fn assert_dma_capable(_buf: &[u8]) {}
