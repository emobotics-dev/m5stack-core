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

// Per-board panic handler + logger backend. Fire27: esp-backtrace over the UART
// console + esp-println. CoreS3: panic-halt (USB-Serial-JTAG, with which
// esp-backtrace/esp-println conflict — it logs over RTT; see shim::init_logger).
#[cfg(feature = "fire27")]
use esp_backtrace as _;
#[cfg(feature = "fire27")]
use esp_println as _;
#[cfg(feature = "cores3")]
use panic_halt as _;

esp_bootloader_esp_idf::esp_app_desc!();

// esp-backtrace's `custom-halt` feature calls this after the backtrace (Fire27).
#[cfg(feature = "fire27")]
#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();
    shim::init_logger();
    let board = board::Board::split(p);
    shim::init_heap_lvgl();
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);
    log::info!("Embassy initialized");

    // Bring up the display (DMA bus, no SD) and the front-panel input together.
    // Fire27 input = the three buttons; CoreS3 = the touch zones (the one I2C
    // bus that resets the panel also drives touch). Same `Input` type after.
    let (dma_rx, dma_tx) = ui::dma_bufs();
    #[cfg(feature = "fire27")]
    let (dbus, input) = board::lvgl_bringup(board.spi2, board.buttons, dma_rx, dma_tx).await;
    #[cfg(feature = "cores3")]
    let (dbus, input) = board::lvgl_bringup(board.spi2, board.i2c0, dma_rx, dma_tx).await;
    let driver = DisplayDriver::new(dbus);
    log::info!("Display initialized");

    // Run the SPI flush on a high-priority interrupt executor (SWI1) so it never
    // blocks the LVGL render loop on the low-priority executor.
    let int_exec = make_static!(InterruptExecutor::new(board.system.sw_int.software_interrupt1));
    let hi_spawner = int_exec.start(Priority::min());
    hi_spawner.spawn(ui::flush_task(driver).expect("spawn flush task"));

    // Decode front-panel events → LVGL keys (feeds `ui::input::KEYPAD`).
    spawner.spawn(ui::input::input_task(input).expect("spawn input task"));

    static mut LVGL_BUFS: LvglBuffers<{ LVGL_BUF_BYTES }> = LvglBuffers::new();
    // SAFETY: `LVGL_BUFS` is touched only here, before the single-threaded LVGL
    // render loop takes exclusive ownership of it for the rest of the program.
    let bufs = unsafe { &mut *core::ptr::addr_of_mut!(LVGL_BUFS) };

    // Event-mode keypad render loop: reads the keypad the moment `input_task`
    // posts a key (raced against the inter-tick sleep via `ui::input::wake`),
    // and routes the view's focus group to the keypad indev.
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
