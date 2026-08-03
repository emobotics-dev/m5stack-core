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
//! mem::init_heap(HeapProfile::Default); // DRAM regions only — never touches PSRAM
//! mem::init_heap(HeapProfile::Lvgl);
//! ```
//!
//! `init_heap` never touches PSRAM — it owns only the DRAM region sizes for a
//! [`HeapProfile`]. PSRAM is a separate, deliberate decision: see
//! [`psram_map`] / [`psram_split`] below. The PSRAM-specific surface (those two,
//! the checked [`psram_box`] / [`psram_vec`], [`PsramSafe`]) needs the `psram`
//! feature; the heap regions and [`dma_buffer`] need only `heap`.
//!
//! Both boards carry SPI PSRAM (Fire27: ~4 MB, CoreS3: ~8 MB). `esp-alloc`
//! exposes a single global heap that can be backed by several regions. Getting
//! PSRAM into one of those regions is opt-in and explicit — there is no
//! function that puts PSRAM behind the global allocator as a side effect of
//! anything else, because once a region carries external capability, *every*
//! plain `alloc::vec!` / `Box` / `String` in the whole crate graph — not just
//! your own code — becomes eligible to silently spill into it once internal
//! DRAM is exhausted (esp-alloc has no "external, but not for capability-less
//! requests" region flag).
//!
//! 1. **[`psram_map`] — the default.** Maps PSRAM and hands back the whole
//!    region as a private slice. Nothing is registered with the global heap;
//!    nothing here is ever reachable by a plain allocation. Hand the slice to
//!    a foreign allocator (e.g. LVGL's TLSF) or use it directly.
//! 2. **[`psram_split`] — deliberate global exposure.** Carves a private
//!    region off the base (as above) *and* registers the remainder with the
//!    global heap, for the checked [`psram_box`] / [`psram_vec`] helpers
//!    (which reject atomic-bearing types at compile time — see [`PsramSafe`]).
//!    Reach for this only when you've decided part (or with `reserve: 0`, all)
//!    of PSRAM should be globally exposed, and accept what that costs.
//!
//! ```ignore
//! use m5stack_core::mem;
//!
//! // Never touches the global allocator:
//! let psram = mem::psram_map(peripherals.PSRAM);
//!
//! // Deliberately expose part of PSRAM globally:
//! let split = mem::psram_split(peripherals.PSRAM, 2 * 1024 * 1024)?;
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
#[cfg(feature = "psram")]
use core::mem::MaybeUninit;

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

/// Register the global heap's DRAM regions for `profile`, using the
/// HIL-proven per-board sizes. Call once, right after [`crate::board::init`] /
/// `Board::split` and before any allocation.
///
/// This is the single place a binary sets up the DRAM heap — it never calls
/// `esp_alloc::heap_allocator!` itself. This never touches PSRAM: see
/// [`psram_map`] / [`psram_split`] for that, called separately (and
/// optionally) afterward. esp-alloc's global heap holds at most three
/// regions; each profile registers at most the reclaimed-ROM region and the
/// plain-DRAM region, leaving room for [`psram_split`]'s external region — a
/// 4th `add_region` panics silently.
pub fn init_heap(profile: HeapProfile) {
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
}

/// Map the board's external PSRAM and return the whole region as a private
/// slice. Nothing is registered with the global heap — nothing returned here
/// is ever reachable by a plain `alloc::vec!` / `Box` / `String`.
///
/// The default way to get at PSRAM. Hand the slice to a foreign allocator
/// (e.g. LVGL's built-in TLSF via `lv_mem_add_pool`) or use it directly. For
/// deliberate global exposure (so [`psram_box`] / [`psram_vec`] can reach
/// part of PSRAM), use [`psram_split`] instead.
///
/// The size is auto-detected. Call once, after [`esp_hal::init`]. Calling
/// this (or [`psram_split`]) more than once is unsound — the PSRAM controller
/// must only be initialized a single time.
#[cfg(feature = "psram")]
pub fn psram_map(psram: PSRAM<'static>) -> &'static mut [MaybeUninit<u8>] {
    // SAFETY: see `psram_split`, which this mirrors with `reserve == total`
    // (the whole region private, nothing registered globally).
    let psram = esp_hal::psram::Psram::new(psram, Default::default());
    let (base, total) = psram.raw_parts();
    info!("PSRAM mapped: {} KiB private", total / 1024);
    unsafe { core::slice::from_raw_parts_mut(base as *mut MaybeUninit<u8>, total) }
}

/// A private PSRAM region carved off the global heap, plus the external bytes
/// registered with the global heap. Returned by [`psram_split`].
#[cfg(feature = "psram")]
pub struct PsramSplit {
    /// A private, exclusive, contiguous PSRAM region for a *foreign* allocator
    /// (e.g. LVGL's built-in TLSF via `lv_mem_add_pool`). It is **not** part of
    /// the global heap, so the global allocator never hands it to `Box` / `Vec`
    /// / DMA. Its base is the PSRAM mapping base, so it is large-aligned (≥ any
    /// reasonable `ALIGN_SIZE`) — the caller needs no alignment math and no
    /// `unsafe`.
    ///
    /// `'static` is sound because esp-hal's `Psram` has no `Drop`: the mapping is
    /// a hardware side effect recorded in esp-hal's range statics and is *not*
    /// undone when the `Psram` value drops.
    pub private: &'static mut [MaybeUninit<u8>],
    /// External (PSRAM) heap free immediately after registering the remainder
    /// with the global heap, in bytes. `0` when `reserve` was `None` (all
    /// private, nothing registered).
    pub global_free: usize,
}

// `private` is a `&'static mut` to uninit memory, so `PsramSplit` cannot derive
// `Debug`; a manual impl prints the base/len/free a consumer wants when bringing
// this up on a new board (`log::info!("{:?}", split)`).
#[cfg(feature = "psram")]
impl core::fmt::Debug for PsramSplit {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PsramSplit")
            .field("private_base", &self.private.as_ptr())
            .field("private_len", &self.private.len())
            .field("global_free", &self.global_free)
            .finish()
    }
}

/// Why [`psram_split`] could not satisfy the request.
#[cfg(feature = "psram")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsramSplitError {
    /// PSRAM did not map — `esp_hal::psram::init_psram` failed, or the board has
    /// none. Nothing was registered with the global heap.
    NotMapped,
    /// PSRAM mapped, but smaller than the requested `reserve`. `available` is the
    /// total mapped size, so the caller can retry with a smaller pool without
    /// re-querying — the `PSRAM` peripheral was consumed by value.
    TooSmall { available: usize },
}

/// Map the board's external PSRAM, carve a **private** region off the base, and
/// register the remainder with the global heap.
///
/// The deliberate-global-exposure counterpart of [`psram_map`] (which makes
/// the whole region private and touches the global heap not at all). Reach for
/// `psram_split` only once you've decided that some of PSRAM should be
/// reachable by the checked [`psram_box`] / [`psram_vec`] helpers — that
/// decision then applies to the *whole* crate graph's plain allocations, not
/// just yours, once internal DRAM runs out (see the module docs).
///
/// - `reserve` carves that many bytes private from the base and registers the
///   remainder globally. `reserve == 0` is the maximal-exposure case: an empty
///   [`PsramSplit::private`] slice and *all* PSRAM registered globally. Sound (a
///   zero-length slice grants access to nothing) but rarely what you want —
///   prefer [`psram_map`] if you don't need any global exposure at all.
/// - The private region is carved **from the base**, so [`PsramSplit::private`]
///   starts at the (large-aligned) PSRAM mapping base — aligned for LVGL's TLSF
///   with no math; esp-alloc aligns the remainder's base internally.
/// - This primitive controls *placement* (the private/global split). The PSRAM
///   **hardware** mapping uses the default [`esp_hal::psram`] config, which
///   auto-detects size — the board-correct choice for both boards. A
///   `psram_split_with(config)` variant is a non-breaking addition if a consumer
///   ever needs a custom `PsramConfig` (a fixed `PsramSize`, say); no current
///   one does.
///
/// Call once, after [`esp_hal::init`], **instead of** [`psram_map`]: taking
/// `PSRAM<'static>` by value makes the once-only mapping a type-level
/// guarantee, so the two cannot both run.
///
/// # Caveats handed back to the caller
/// - **No atomics in the private region.** The checked [`psram_box`] /
///   [`psram_vec`] cannot guard a foreign allocator, so keeping `Atomic*` out of
///   whatever is placed here is the caller's responsibility (holds for LVGL while
///   `LV_USE_OS` is `LV_OS_NONE`). See [`PsramSafe`].
/// - **DMA.** The ESP32 (Fire27) cannot DMA to/from PSRAM at all; the ESP32-S3
///   can but slowly. A foreign allocator must not place DMA'd buffers here. See
///   [`assert_dma_capable`] / [`dma_buffer`].
///
/// # Errors
/// [`PsramSplitError::NotMapped`] if PSRAM does not map; [`PsramSplitError::TooSmall`]
/// if it maps smaller than `reserve`.
#[cfg(feature = "psram")]
pub fn psram_split(psram: PSRAM<'static>, reserve: usize) -> Result<PsramSplit, PsramSplitError> {
    // `Psram` has no `Drop`: the mapping is recorded in esp-hal's range statics
    // and survives the value dropping (see `PsramSplit::private`), so a local is
    // fine — nothing unmaps at the end of this block.
    let psram = esp_hal::psram::Psram::new(psram, Default::default());
    let (base, total) = psram.raw_parts();

    // `Psram::new` maps only if `init_psram` succeeded; on failure the range
    // statics stay unset and `raw_parts` reports a zero-size region.
    if total == 0 {
        return Err(PsramSplitError::NotMapped);
    }

    if reserve > total {
        return Err(PsramSplitError::TooSmall { available: total });
    }

    // Carve `[base, base + reserve)` private.
    // SAFETY: `base` is the exclusively-owned, `'static` PSRAM mapping (once-only,
    // guaranteed by consuming `PSRAM<'static>` by value). This sub-range is handed
    // out privately and is never registered with the global heap, so no aliasing.
    // `MaybeUninit<u8>` has align 1, and `reserve <= total <= isize::MAX`. When
    // `reserve == 0` the slice is empty: its base pointer lies inside the region
    // that *is* registered globally below, but a zero-length slice dereferences
    // nothing, so it still aliases nothing.
    let private =
        unsafe { core::slice::from_raw_parts_mut(base as *mut MaybeUninit<u8>, reserve) };

    // Register the remainder `[base + reserve, base + total)` with the global heap.
    let global_free = if reserve < total {
        // SAFETY: disjoint from `private`, `'static`, exclusively the heap's, and
        // `total - reserve > 0` here (so esp-alloc's `size > 0` precondition holds).
        unsafe {
            esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
                base.add(reserve),
                total - reserve,
                esp_alloc::MemoryCapability::External.into(),
            ));
        }
        esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::External.into())
    } else {
        0
    };

    info!(
        "PSRAM split: {} KiB private, {} KiB external free (global)",
        reserve / 1024,
        global_free / 1024
    );
    Ok(PsramSplit { private, global_free })
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
    use core::sync::atomic::Atomic;

    // Every atomic aliases `Atomic<T>`. Enumerating the aliases instead is
    // incomplete, and illegal since each names a concrete `Atomic<..>` — a
    // negative impl may not specialize (E0366).
    impl<T> !PsramSafe for Atomic<T> {}

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

/// Free bytes in the global heap's **internal** DRAM regions (reclaimed-ROM +
/// plain-DRAM), right now. The tight resource on both boards; use it to measure
/// headroom (e.g. before/after moving a subsystem's heap to PSRAM).
pub fn internal_free() -> usize {
    esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into())
}

/// Free bytes in the global heap's **external** (PSRAM) region, right now — `0`
/// unless [`psram_split`] registered a non-empty remainder globally (a private
/// [`psram_map`] / [`psram_split`] region is *not* counted here).
pub fn external_free() -> usize {
    esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::External.into())
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
