// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unified front-panel event readout (no radio).
//!
//! Fire27 reads the three physical buttons; CoreS3 reads the FT6336U touch
//! strip — both surface the same [`ButtonEvent`](demos::board::ButtonEvent),
//! so the readout loop is identical.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use common::{STRIP_BYTES, draw_panel};
use demos::board::{self, ButtonAction, ButtonId, INPUT_KIND, NAME};
use demos::shim;
use embassy_executor::Spawner;
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

    #[cfg(feature = "fire27")]
    let mut input = board::Input::new(board.buttons);
    #[cfg(feature = "cores3")]
    let mut input = board::Input::new(_i2c);

    // Last action seen on each of the three positions. The point of the demo is
    // that a single tap, a multi-tap (`x2`/`x3`/…), and a long press are all
    // distinct `ButtonEvent`s — identical on Fire27 buttons and CoreS3 touch.
    let mut slots: [alloc::string::String; 3] =
        core::array::from_fn(|_| alloc::string::String::from("idle"));
    render(&mut display, &mut strip_buf[..], &slots).await;

    loop {
        let ev = input.next_event().await;
        log::info!("input: {:?} {:?}", ev.id, ev.action);
        let action = match ev.action {
            ButtonAction::Short(1) => alloc::string::String::from("tap"),
            ButtonAction::Short(n) => alloc::format!("tap x{}", n),
            ButtonAction::Long => alloc::string::String::from("HELD (long)"),
        };
        slots[match ev.id {
            ButtonId::Left => 0,
            ButtonId::Center => 1,
            ButtonId::Right => 2,
        }] = action;
        render(&mut display, &mut strip_buf[..], &slots).await;
    }
}

/// Render the three-position last-action readout through the shared panel.
async fn render(display: &mut board::Lcd, strip_buf: &mut [u8], slots: &[alloc::string::String; 3]) {
    let l = alloc::format!("Left  : {}", slots[0]);
    let c = alloc::format!("Center: {}", slots[1]);
    let r = alloc::format!("Right : {}", slots[2]);
    draw_panel(
        display,
        strip_buf,
        NAME,
        INPUT_KIND,
        &["tap / multi-tap / hold:", "", &l, &c, &r],
    )
    .await;
}
