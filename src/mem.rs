// SPDX-License-Identifier: MIT OR Apache-2.0
//! Global heap ownership + external PSRAM integration.
//!
//! The BSP owns the global heap so a binary never spells out
//! `esp_alloc::heap_allocator!` or the per-board region sizes itself:
//! [`init_heap`] declares the esp-alloc DRAM regions for a [`HeapProfile`] and,
//! optionally, registers external PSRAM. esp-alloc's global heap holds at most
//! three regions, so each profile registers the reclaimed-ROM region, the
//! plain-DRAM region, and (when PSRAM is supplied) the external region — never a
//! fourth (a 4th `add_region` panics silently). The sizes are the HIL-proven
//! per-board values previously copied into every binary.
//!
//! ```ignore
//! use m5stack_core::mem::{self, HeapProfile};
//!
//! // After board init, before the first allocation:
//! mem::init_heap(HeapProfile::Default, Some(board.psram)); // DRAM + PSRAM
//! mem::init_heap(HeapProfile::Lvgl, None);                  // DRAM only
//! ```
//!
//! The PSRAM-specific surface below ([`init_psram_heap`], the checked
//! [`psram_box`] / [`psram_vec`], [`PsramSafe`]) needs the `psram` feature; the
//! heap regions and [`dma_buffer`] need only `heap`.
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

use allocator_api2::vec::Vec;
pub use esp_alloc::{AnyMemory, ExternalMemory, InternalMemory};
use esp_hal::peripherals::PSRAM;
use esp_hal::ram;

#[cfg(feature = "psram")]
use allocator_api2::boxed::Box;

/// Heap size profile — selects the HIL-proven per-board DRAM region sizes for a
/// workload. The BSP owns the sizes so every binary gets the validated values;
/// pass the matching profile to [`init_heap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapProfile {
    /// Display / I2C / WiFi-STA workloads: reclaimed-ROM + plain DRAM (+ PSRAM).
    Default,
    /// LVGL UI: the reclaimed-ROM region only (LVGL object/style pool); no PSRAM.
    Lvgl,
    /// WiFi + BLE coexistence: more controller heap (+ PSRAM). Fire27 favours the
    /// reclaimed region; CoreS3 favours plain DRAM.
    Coex,
}

/// Register the global heap regions for `profile`, plus external PSRAM when
/// `psram` is `Some`, using the HIL-proven per-board sizes. Call once, right
/// after [`crate::board::init`] / `Board::split` and before any allocation.
///
/// This is the single place a binary sets up the heap — it never calls
/// `esp_alloc::heap_allocator!` itself. esp-alloc's global heap holds at most
/// three regions; each profile registers at most the reclaimed-ROM region, the
/// plain-DRAM region and the external PSRAM region — never a fourth (a 4th
/// `add_region` panics silently). Pass `None` for heap-only workloads
/// (e.g. [`HeapProfile::Lvgl`], or a board with no external RAM).
///
/// Registering PSRAM needs the `psram` feature; without it a `Some(_)` argument
/// is accepted but the external region is **not** added (the DRAM regions still
/// are).
pub fn init_heap(profile: HeapProfile, psram: Option<PSRAM<'static>>) {
    match profile {
        HeapProfile::Default => {
            esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
            esp_alloc::heap_allocator!(size: 64 * 1024);
        }
        HeapProfile::Lvgl => {
            esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
        }
        HeapProfile::Coex => {
            #[cfg(feature = "fire27")]
            {
                esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 96 * 1024);
                esp_alloc::heap_allocator!(size: 24 * 1024);
            }
            #[cfg(feature = "cores3")]
            {
                esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
                esp_alloc::heap_allocator!(size: 96 * 1024);
            }
        }
    }
    if let Some(psram) = psram {
        // `psram` feature off → the binding is consumed here without registering
        // an external region (the PSRAM controller stays uninitialised).
        let _ = &psram;
        #[cfg(feature = "psram")]
        init_psram_heap(psram);
    }
}

/// Map the board's external PSRAM and add it to the global heap as an
/// [`ExternalMemory`] region.
///
/// The size is auto-detected. Returns the amount of external (PSRAM) heap free
/// immediately after registration, in bytes.
///
/// Call once, after [`esp_hal::init`]. Usually invoked for you by [`init_heap`]
/// when you pass `Some(psram)`; call it directly only if you manage the DRAM
/// regions yourself. Calling it more than once is unsound — the PSRAM
/// controller must only be initialized a single time.
#[cfg(feature = "psram")]
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
#[cfg(feature = "psram")]
pub unsafe auto trait PsramSafe {}

#[cfg(feature = "psram")]
mod psram_safe_impls {
    use super::PsramSafe;
    use core::sync::atomic::{
        AtomicBool, AtomicI8, AtomicI16, AtomicI32, AtomicIsize, AtomicPtr, AtomicU8, AtomicU16,
        AtomicU32, AtomicUsize,
    };

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
}

/// Allocate `value` in external PSRAM. Atomic-bearing `T` is rejected at
/// compile time via [`PsramSafe`].
#[cfg(feature = "psram")]
pub fn psram_box<T: PsramSafe>(value: T) -> Box<T, ExternalMemory> {
    Box::new_in(value, ExternalMemory)
}

/// A `Vec<T>` with room for `capacity` elements reserved in external PSRAM.
/// Atomic-bearing `T` is rejected at compile time via [`PsramSafe`].
#[cfg(feature = "psram")]
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
