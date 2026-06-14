// SPDX-License-Identifier: MIT OR Apache-2.0
//! LVGL (oxivgl) interactive UI demo: three focusable buttons navigated by the
//! front-panel input — `PREV`/`NEXT` move focus, `ENTER` clicks — plus a live
//! frame counter. Identical on both boards: Fire27 drives it from the three
//! physical buttons, CoreS3 from the FT6336U touch zones, both through the
//! BSP's unified `ButtonEvent` → an LVGL keypad indev (`demos::ui::input`).
//!
//! The per-board bring-up lives in `board::lvgl_bringup`; the flush glue, view,
//! and input adapter live in `demos::ui`, so this `main` only wires them. Build
//! with `--features lvgl`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

use demos::board;
use demos::shim;
use demos::ui::{self, DisplayDriver, LVGL_BUF_BYTES, MenuView, SCREEN_H, SCREEN_W};
use embassy_executor::Spawner;
use esp_hal::interrupt::Priority;
use esp_rtos::embassy::InterruptExecutor;
use oxivgl::display::LvglBuffers;
use oxivgl::view::run_app_nav_keypad_events;
use static_cell::make_static;

// Panic handler + log/console transport come from the BSP (the panic-handler +
// console-serial features); the app descriptor is the one line the binary keeps.
m5stack_core::app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();
    let board = board::Board::split(p);
    shim::init_heap_lvgl();
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);
    #[cfg(feature = "fire27")]
    let _console =
        shim::init_console(spawner, board::console_serial(board.uart0, board.uart0_rx, board.uart0_tx));
    #[cfg(feature = "cores3")]
    let _console = shim::init_console(spawner, board::console_serial(board.usb_device));
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
