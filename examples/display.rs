// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unified front-panel event readout (no radio).
//!
//! Fire27 reads the three physical buttons; CoreS3 reads the FT6336U touch
//! strip — both surface the same [`ButtonEvent`](common::board::ButtonEvent),
//! so the readout loop is identical.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[path = "common/mod.rs"]
mod common;

extern crate alloc;

use crate::common::helpers::{STRIP_BYTES, draw_panel};
use crate::common::board::{self, ButtonAction, ButtonId, INPUT_KIND, NAME};
use embassy_executor::Spawner;
use static_cell::make_static;

// Panic handler + log/console transport come from the BSP (the panic-handler +
// console-serial features); the app descriptor is the one line the binary keeps.
m5stack_core::app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    common::boot!(spawner, board, Default);

    let (mut display, _i2c) = board::init_display(board.spi2, board.i2c0).await;

    // #32 I2: a consumer queries the board's input capability rather than
    // assuming a layout. Here we just report it; a real UI would install the
    // matching indev (keypad vs pointer) from this.
    log::info!("input caps: {:?}", board::input_caps());

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
