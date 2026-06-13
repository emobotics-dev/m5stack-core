// SPDX-License-Identifier: MIT OR Apache-2.0
//! Display splash + unified front-panel event readout (no radio).
//!
//! Fire27 reads the three physical buttons; CoreS3 reads the FT6336U touch
//! strip — both surface the same [`ButtonEvent`](demos::board::ButtonEvent),
//! so the readout loop is identical.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use demos::board::{self, NAME};
use demos::shim;
use embassy_executor::Spawner;
use common::{STRIP_BYTES, draw_demo, draw_status};
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
    shim::init_heaps_default(board.psram);
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);

    let (mut display, _i2c) = board::init_display(board.spi2, board.i2c0).await;

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);
    draw_demo(&mut display, &mut strip_buf[..], NAME, &["display demo"]).await;

    #[cfg(feature = "fire27")]
    let mut input = board::Input::new(board.buttons);
    #[cfg(feature = "cores3")]
    let mut input = board::Input::new(_i2c);

    let title = alloc::format!("{} input", NAME);
    draw_status(
        &mut display,
        &mut strip_buf[..],
        &["m5stack-core demo", &title, "", "last event:", "(waiting)"],
    )
    .await;
    loop {
        let ev = input.next_event().await;
        log::info!("button: {:?} {:?}", ev.id, ev.action);
        let last = alloc::format!("{:?}: {:?}", ev.id, ev.action);
        draw_status(
            &mut display,
            &mut strip_buf[..],
            &["m5stack-core demo", &title, "", "last event:", &last],
        )
        .await;
    }
}
