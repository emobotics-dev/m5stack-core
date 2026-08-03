// SPDX-License-Identifier: MIT OR Apache-2.0
//! LVGL (oxivgl) interactive UI demo: three focusable buttons navigated by the
//! front-panel input — `PREV`/`NEXT` move focus, `ENTER` clicks — plus a live
//! frame counter. Identical on both boards: Fire27 drives it from the three
//! physical buttons, CoreS3 from the FT6336U touch zones, both through the
//! BSP's unified `ButtonEvent` → an LVGL keypad indev (`common::ui::input`).
//!
//! The per-board bring-up lives in `board::lvgl_bringup`; the flush glue, view,
//! and input adapter live in `common::ui`, so this `main` only wires them. Build
//! with `--features lvgl`.
//!
//! Render and flush share one executor here, which is the arrangement #63 was
//! filed against: it holds ~30 fps but makes everything else on that executor
//! wait milliseconds for a panel transfer. Kept as the input/widget demo it
//! always was — for the pipeline a real application should use, see
//! `lvgl_threads.rs` and `docs/lvgl-ui-performance.md`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[path = "../common/mod.rs"]
mod common;
mod view;

use crate::common::board;
use crate::common::ui::{self, DisplayDriver, LVGL_BUF_BYTES, SCREEN_H, SCREEN_W};
use crate::view::MenuView;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::interrupt::Priority;
use esp_rtos::embassy::InterruptExecutor;
use oxivgl::display::LvglBuffers;
use oxivgl::view::run_app_nav_keypad_events;
use static_cell::make_static;

// Panic handler + log/console transport come from the BSP (the panic-handler +
// console-serial features); the app descriptor is the one line the binary keeps.
m5stack_core::app_desc!();

/// Route an LVGL assertion (a failed `lv_malloc`, a NULL object, a corrupt style)
/// through the Rust `#[panic_handler]` — a loud `[PANIC]` (esp-backtrace + halt,
/// RWDT recovers) instead of LVGL's default `LV_ASSERT_HANDLER while(1);`, which
/// spins the LVGL task forever with no message, no backtrace and no reset (#57).
/// LVGL emits `LV_LOG_ERROR` with the failing expression/file/line immediately
/// before calling this, so the diagnosis lands directly above the panic.
///
/// `lv_conf.h` declares the prototype and points `LV_ASSERT_HANDLER` here. oxivgl
/// 0.5.0 provides no such symbol, so the demo defines its own (name-spaced to
/// avoid the unprefixed `lv_assert_handler` that other consumers export).
#[unsafe(no_mangle)]
pub extern "C" fn demos_lv_assert_handler() {
    panic!("LVGL assertion failed — see the LV_LOG_ERROR line above (expr/file/line)");
}

/// Log internal-DRAM free once at start, then every 10 s with the delta from the
/// first reading (drift). See #49.
#[embassy_executor::task]
async fn heap_stats_task() {
    let base = m5stack_core::mem::internal_free();
    log::info!("[heap] internal free baseline: {} B", base);
    let mut n = 0u32;
    loop {
        Timer::after(Duration::from_secs(10)).await;
        n += 1;
        let now = m5stack_core::mem::internal_free();
        log::info!(
            "[heap] t={}s internal free: {} B (drift {} B)",
            n * 10,
            now,
            now as i32 - base as i32,
        );
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    common::boot!(spawner, board, Lvgl);
    log::info!("Embassy initialized");

    // Bring up the display (DMA bus, no SD) + the input. Fire27 = three buttons.
    // CoreS3 = the FT6336U: the bottom-strip *keys* drive the keypad (BSP
    // `TouchButtons`, multi-tap/long-press) AND on-screen taps drive a POINTER —
    // the one I2C bus resets the panel and is shared by both.
    let (dma_rx, dma_tx) = ui::dma_bufs();
    #[cfg(feature = "fire27")]
    let (dbus, input) = board::lvgl_bringup(board.spi2, board.buttons, dma_rx, dma_tx).await;
    #[cfg(feature = "cores3")]
    let (dbus, i2c) = board::lvgl_bringup(board.spi2, board.i2c0, dma_rx, dma_tx).await;
    // CoreS3: the bottom-strip touch buttons (keypad source), on the same bus.
    #[cfg(feature = "cores3")]
    let input = board::Input::new(i2c);
    let driver = DisplayDriver::new(dbus);
    log::info!("Display initialized");

    // Run the SPI flush on a high-priority interrupt executor (SWI1) so it never
    // blocks the LVGL render loop on the low-priority executor.
    let int_exec = make_static!(InterruptExecutor::new(board.system.sw_int.software_interrupt1));
    let hi_spawner = int_exec.start(Priority::min());
    hi_spawner.spawn(ui::flush_task(driver).expect("spawn flush task"));

    // #49 instrumentation: log internal-DRAM free periodically so the
    // LVGL-on-internal-DRAM baseline (and its steady-state drift) can be read off
    // HIL. This is the "before" half of the psram_split A/B; the "after" needs
    // oxivgl `reserve_pool` to move LVGL's heap off internal DRAM.
    spawner.spawn(heap_stats_task().expect("spawn heap stats task"));

    // Both boards: decode the front-panel buttons → LVGL keys (keypad indev).
    // On CoreS3 these are the bottom-strip keys via the BSP button API.
    spawner.spawn(ui::input::input_task(input).expect("spawn input task"));
    // CoreS3 additionally: poll the FT6336U → the on-screen POINTER indev.
    #[cfg(feature = "cores3")]
    spawner.spawn(ui::input::touch_poll_task(i2c).expect("spawn touch poll task"));

    static mut LVGL_BUFS: LvglBuffers<{ LVGL_BUF_BYTES }> = LvglBuffers::new();
    // SAFETY: `LVGL_BUFS` is touched only here, before the single-threaded LVGL
    // render loop takes exclusive ownership of it for the rest of the program.
    let bufs = unsafe { &mut *core::ptr::addr_of_mut!(LVGL_BUFS) };

    // Event-mode keypad render loop (reads the keypad the moment a key is posted,
    // routes the view's focus group to it). On CoreS3 the POINTER indev
    // (registered in `MenuView::create`) is also polled by LVGL during
    // `lv_timer_handler`, so on-screen taps work alongside the bottom keys.
    run_app_nav_keypad_events(
        SCREEN_W.into(),
        SCREEN_H.into(),
        bufs,
        MenuView::default(),
        &ui::input::KEYPAD,
        ui::input::wake,
    )
    .await
}
