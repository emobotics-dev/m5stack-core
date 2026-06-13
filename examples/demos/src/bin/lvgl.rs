// SPDX-License-Identifier: MIT OR Apache-2.0
//! LVGL (oxivgl) UI demo: a title, an animated spinner, and a frame counter.
//!
//! The per-board display/SD bring-up lives in the BSP (`board::spi2`); the
//! oxivgl flush glue, the demo view, and (Fire27) the keypad indev live in
//! `demos::ui`, so this `main` only wires them. Build with `--features lvgl`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

use demos::board;
use demos::shim;
use demos::ui::{self, DemoView, DisplayDriver, LVGL_BUF_BYTES, SCREEN_H, SCREEN_W};
use embassy_executor::Spawner;
use esp_hal::interrupt::Priority;
use esp_rtos::embassy::InterruptExecutor;
use oxivgl::display::LvglBuffers;
use oxivgl::view::run_app;
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
async fn main(_spawner: Spawner) {
    let p = board::init();
    shim::init_logger();
    let board = board::Board::split(p);
    shim::init_heap_lvgl();
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);
    log::info!("Embassy initialized");

    // Bring up the display on the descriptor-backed DMA bus (no SD). CoreS3
    // resets/powers the panel via I2C inside this call; Fire27 ignores i2c0.
    let (dma_rx, dma_tx) = ui::dma_bufs();
    let dbus = board::lvgl_display(board.spi2, board.i2c0, dma_rx, dma_tx).await;
    let driver = DisplayDriver::new(dbus);
    log::info!("Display initialized");

    // Fire27 only: the three front-panel buttons → LVGL keypad indev. CoreS3 is
    // touch-only and registers no keypad indev.
    #[cfg(feature = "fire27")]
    ui::input::spawn(
        _spawner,
        board.buttons.left,
        board.buttons.center,
        board.buttons.right,
    );

    // Run the SPI flush on a high-priority interrupt executor (SWI1) so it never
    // blocks the LVGL render loop on the low-priority executor.
    let int_exec = make_static!(InterruptExecutor::new(board.system.sw_int.software_interrupt1));
    let hi_spawner = int_exec.start(Priority::min());
    hi_spawner.spawn(ui::flush_task(driver).expect("spawn flush task"));

    static mut LVGL_BUFS: LvglBuffers<{ LVGL_BUF_BYTES }> = LvglBuffers::new();
    // SAFETY: `LVGL_BUFS` is touched only here, before the single-threaded LVGL
    // render loop takes exclusive ownership of it for the rest of the program.
    let bufs = unsafe { &mut *core::ptr::addr_of_mut!(LVGL_BUFS) };

    run_app::<DemoView, { LVGL_BUF_BYTES }>(SCREEN_W.into(), SCREEN_H.into(), bufs, DemoView::default()).await
}
