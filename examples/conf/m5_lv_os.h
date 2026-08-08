/* SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * LVGL OS port types (LV_OS_CUSTOM). The functions themselves live in
 * `demos::ui::lvos`; LVGL declares them itself in lv_os_private.h, so only the
 * handle types belong here.
 *
 * Each is one pointer: an esp-rtos task or semaphore, leaked so it outlives
 * LVGL's use of it.
 */
#ifndef M5_LV_OS_H
#define M5_LV_OS_H

typedef struct {
    void *task;
} lv_thread_t;

typedef struct {
    void *sem;
} lv_mutex_t;

typedef struct {
    void *sem;
} lv_thread_sync_t;

#endif /* M5_LV_OS_H */
