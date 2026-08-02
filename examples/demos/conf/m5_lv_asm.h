/* SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * LVGL software-blend hooks (LV_DRAW_SW_ASM_CUSTOM), implemented in
 * `demos::ui::lvasm`.
 *
 * Espressif's `lvgl_s3_simd_patch` overrides the RGB565 hooks. We render
 * RGB565_SWAPPED, which LVGL dispatches to a different file with its own
 * hook names, so that patch is never called for us — these are the ones that
 * matter here.
 *
 * Each returns `lv_result_t`: 1 (LV_RESULT_OK) when we handled the blend,
 * 0 (LV_RESULT_INVALID) to fall through to LVGL's own loop. That fallback is
 * what makes partial implementations safe — cover the cases worth covering and
 * decline the rest.
 */
#ifndef M5_LV_ASM_H
#define M5_LV_ASM_H

#include <stdint.h>

uint32_t m5_blend_fill_swapped(const void *dsc);
uint32_t m5_blend_fill_swapped_opa(const void *dsc);
uint32_t m5_blend_fill_swapped_mask(const void *dsc);
uint32_t m5_blend_fill_swapped_mix(const void *dsc);

#define LV_DRAW_SW_COLOR_BLEND_TO_RGB565_SWAPPED(dsc)              m5_blend_fill_swapped(dsc)
#define LV_DRAW_SW_COLOR_BLEND_TO_RGB565_SWAPPED_WITH_OPA(dsc)     m5_blend_fill_swapped_opa(dsc)
#define LV_DRAW_SW_COLOR_BLEND_TO_RGB565_SWAPPED_WITH_MASK(dsc)    m5_blend_fill_swapped_mask(dsc)
#define LV_DRAW_SW_COLOR_BLEND_TO_RGB565_SWAPPED_MIX_MASK_OPA(dsc) m5_blend_fill_swapped_mix(dsc)

#endif /* M5_LV_ASM_H */
