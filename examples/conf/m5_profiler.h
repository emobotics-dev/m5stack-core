/* SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * LVGL profiler shim. `lv_conf.h` points LV_PROFILER_BEGIN/END here so LVGL's
 * own instrumentation reports into `demos::ui::lvprof` instead of its builtin
 * profiler, which wants a filesystem and a printf.
 *
 * Tags are `__func__` or literals, so the pointer is stable and is used as the
 * key — no string compare on the hot path.
 */
#ifndef M5_PROFILER_H
#define M5_PROFILER_H

void m5_prof_begin(const char *tag);
void m5_prof_end(const char *tag);

#endif /* M5_PROFILER_H */
