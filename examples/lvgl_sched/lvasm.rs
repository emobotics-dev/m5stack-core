// SPDX-License-Identifier: MIT OR Apache-2.0
//! Software-blend hooks for LVGL's RGB565_SWAPPED destination format.
//!
//! Espressif's ESP32-S3 assembly overrides the **RGB565** hooks. We render
//! RGB565_SWAPPED — LVGL dispatches that to a separate file with separate hook
//! names — so their patch is never called here. These fill the swapped ones.
//!
//! Returning [`LV_RESULT_INVALID`] hands the blend back to LVGL's own loop, so
//! an unimplemented case costs nothing but the call. That is what lets this
//! start as pure instrumentation and grow one case at a time.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// `lv_result_t`.
const LV_RESULT_INVALID: u32 = 0;
const LV_RESULT_OK: u32 = 1;

/// LVGL's packed-RGB565 mask: R, G and B spread across a 32-bit word so one
/// multiply interpolates all three. `(c | c << 16) & MIX_MASK`.
const MIX_MASK: u32 = 0x07E0_F81F;

/// Mirrors `lv_draw_sw_blend_fill_dsc_t`. `lv_color_t` is three bytes (b, g, r)
/// in LVGL 9, so `opa` lands at offset 27 — verified against
/// `lv_draw_sw_blend_private.h`.
#[repr(C)]
pub struct FillDsc {
    pub dest_buf: *mut u16,
    pub dest_w: i32,
    pub dest_h: i32,
    pub dest_stride: i32,
    pub mask_buf: *const u8,
    pub mask_stride: i32,
    pub color: [u8; 3],
    pub opa: u8,
}

/// Call counts and pixel totals per hook, so the distribution can be read off a
/// live run rather than guessed. Order: plain, opa, mask, mix.
pub static CALLS: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];
pub static PIXELS: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];

fn account(slot: usize, dsc: *const FillDsc) {
    CALLS[slot].fetch_add(1, Relaxed);
    // SAFETY: LVGL passes a valid descriptor for the duration of the call.
    let d = unsafe { &*dsc };
    PIXELS[slot].fetch_add((d.dest_w * d.dest_h).max(0) as u32, Relaxed);
}

#[unsafe(no_mangle)]
pub extern "C" fn m5_blend_fill_swapped(dsc: *const FillDsc) -> u32 {
    account(0, dsc);
    LV_RESULT_INVALID
}

#[unsafe(no_mangle)]
pub extern "C" fn m5_blend_fill_swapped_opa(dsc: *const FillDsc) -> u32 {
    account(1, dsc);
    LV_RESULT_INVALID
}

/// Solid colour through an alpha mask, RGB565_SWAPPED destination — the hot
/// path for anti-aliased shapes, and by measurement ~2100 calls/s against 62
/// for every other variant combined.
///
/// LVGL's own loop is not naive: its mix is already the one-multiply packed
/// trick. What it does pay, per pixel, is an out-of-line call to
/// `lv_color_16_16_mix` and a recomputation of the packed *foreground* — even
/// though a fill blends one constant colour for the whole span. Hoisting that
/// out of the loop and inlining the mix is the whole idea; the byte swaps stay
/// because the swapped layout splits green across both ends of the word, which
/// leaves the packed trick no headroom.
#[esp_hal::ram]
#[unsafe(no_mangle)]
pub extern "C" fn m5_blend_fill_swapped_mask(dsc: *const FillDsc) -> u32 {
    account(2, dsc);
    // SAFETY: LVGL passes a valid descriptor for the duration of the call.
    let d = unsafe { &*dsc };
    if d.dest_buf.is_null() || d.mask_buf.is_null() || d.dest_w <= 0 || d.dest_h <= 0 {
        return LV_RESULT_INVALID;
    }

    // `lv_color_t` is {blue, green, red}.
    let (b, g, r) = (d.color[0] as u16, d.color[1] as u16, d.color[2] as u16);
    let c16 = ((r & 0xF8) << 8) | ((g & 0xFC) << 3) | (b >> 3);
    let c_swapped = c16.swap_bytes();
    // Hoisted: constant for every pixel of this fill.
    let fg = ((c16 as u32) | ((c16 as u32) << 16)) & MIX_MASK;

    let (w, h) = (d.dest_w as usize, d.dest_h as usize);
    let mut row = d.dest_buf as *mut u8;
    let mut mask = d.mask_buf;

    // One partial pixel. Kept as a closure so the pairwise loop below stays
    // about the *skipping*, which is where the time actually goes.
    let blend_one = |px: *mut u16, m: u8| {
        if m == 0 {
            return;
        }
        if m == 255 {
            unsafe { *px = c_swapped };
            return;
        }
        let dst = unsafe { *px }.swap_bytes();
        let a = ((m as u32) + 4) >> 3;
        let bg = ((dst as u32) | ((dst as u32) << 16)) & MIX_MASK;
        let mixed = ((fg.wrapping_sub(bg).wrapping_mul(a) >> 5).wrapping_add(bg)) & MIX_MASK;
        let out = ((mixed >> 16) as u16) | (mixed as u16);
        unsafe { *px = out.swap_bytes() };
    };

    for _ in 0..h {
        let px_row = row as *mut u16;
        let mut x = 0usize;

        // The mask must be 2-byte aligned before it can be read in pairs.
        if w > 0 && (mask as usize) & 1 != 0 {
            blend_one(unsafe { px_row.add(0) }, unsafe { *mask });
            x = 1;
        }

        // Two mask bytes per test. An anti-aliased edge is a thin band: most
        // pairs are wholly inside (0xFFFF) or wholly outside (0), and those
        // cost one compare for two pixels instead of two.
        while x + 1 < w {
            // SAFETY: `mask + x` is 2-byte aligned by the fixup above, and
            // x + 1 < w keeps both bytes inside the row.
            let pair = unsafe { *(mask.add(x) as *const u16) };
            if pair == 0xFFFF {
                unsafe {
                    *px_row.add(x) = c_swapped;
                    *px_row.add(x + 1) = c_swapped;
                }
            } else if pair != 0 {
                blend_one(unsafe { px_row.add(x) }, unsafe { *mask.add(x) });
                blend_one(unsafe { px_row.add(x + 1) }, unsafe { *mask.add(x + 1) });
            }
            x += 2;
        }
        while x < w {
            blend_one(unsafe { px_row.add(x) }, unsafe { *mask.add(x) });
            x += 1;
        }

        // Both strides are byte counts (`drawbuf_next_row` is a byte add).
        row = unsafe { row.add(d.dest_stride as usize) };
        mask = unsafe { mask.add(d.mask_stride as usize) };
    }
    LV_RESULT_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn m5_blend_fill_swapped_mix(dsc: *const FillDsc) -> u32 {
    account(3, dsc);
    LV_RESULT_INVALID
}

/// Log the per-hook distribution and reset.
pub fn report() {
    const NAMES: [&str; 4] = ["plain", "opa", "mask", "mix"];
    for i in 0..4 {
        let calls = CALLS[i].swap(0, Relaxed);
        let px = PIXELS[i].swap(0, Relaxed);
        if calls > 0 {
            log::info!("[lvasm] {:<6} calls={} px={} px/call={}", NAMES[i], calls, px, px / calls);
        }
    }
}
