// SPDX-License-Identifier: MIT OR Apache-2.0
//! Chip-agnostic helpers shared by the examples.
//!
//! Deliberately depends on neither the BSP nor any esp-hal
//! crate and takes no chip feature: it holds only the pure-`no_std`, reusable
//! pieces (the colour wheel, the strip-rendered display routines, the I2C bus
//! scan, and the shared display geometry constants). Per-board chip-specific
//! bring-up lives in each example's own `lib.rs`.

use embedded_graphics::{
    draw_target::DrawTarget,
    mono_font::{MonoTextStyle, ascii::FONT_9X18_BOLD},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::Text,
};
use embedded_hal::digital::OutputPin;
use lcd_async::{Display, raw_framebuf::RawFrameBuf};

/// Panel width in pixels (both boards use a 320x240 ILI9342C).
pub const W: usize = 320;
/// Panel height in pixels.
pub const H: usize = 240;
/// Height of one render strip in pixels. The framebuffer is rendered a strip at
/// a time so it fits in internal RAM (the ESP32 cannot DMA from PSRAM).
pub const STRIP_H: usize = 40;
/// Byte size of one strip framebuffer (`W * STRIP_H` pixels at 2 bytes/pixel).
pub const STRIP_BYTES: usize = W * STRIP_H * 2;

/// Map `0..=255` to a colour wheel (red → green → blue → red).
///
/// Returns a plain `(r, g, b)` tuple so this crate needs no driver types; the
/// bins map it to their `Rgb` LED colour. The three primary anchor points are
/// `0 → (255, 0, 0)`, `85 → (0, 255, 0)`, `170 → (0, 0, 255)`.
pub fn wheel(pos: u8) -> (u8, u8, u8) {
    let p = pos % 255;
    if p < 85 {
        (255 - p * 3, p * 3, 0)
    } else if p < 170 {
        let p = p - 85;
        (0, 255 - p * 3, p * 3)
    } else {
        let p = p - 170;
        (p * 3, 0, 255 - p * 3)
    }
}

/// Render a list of text lines full-screen using the reused strip framebuffer.
///
/// `strip_buf` must be in internal RAM (it is the SPI/DMA source — the ESP32
/// cannot DMA from PSRAM) and at least [`STRIP_BYTES`] long.
pub async fn draw_status<DI, RST: OutputPin>(
    display: &mut Display<DI, lcd_async::models::ILI9342CRgb565, RST>,
    strip_buf: &mut [u8],
    lines: &[&str],
) where
    DI: lcd_async::interface::Interface<Word = u8>,
{
    let white = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE);
    for strip in 0..(H / STRIP_H) {
        let y_offset = (strip * STRIP_H) as i32;
        {
            let mut fb = RawFrameBuf::<Rgb565, _>::new(&mut strip_buf[..], W, STRIP_H);
            fb.clear(Rgb565::new(0, 0, 4)).ok();
            for (i, line) in lines.iter().enumerate() {
                let y = 18 + i as i32 * 18;
                Text::new(line, Point::new(8, y - y_offset), white)
                    .draw(&mut fb)
                    .ok();
            }
        }
        display
            .show_raw_data(
                0,
                (strip * STRIP_H) as u16,
                W as u16,
                STRIP_H as u16,
                strip_buf,
            )
            .await
            .ok();
    }
}

/// Render a unified status panel: a cyan header (`board` on the left, `title`
/// right-aligned) with an underline, then the white `body` lines below. The
/// sensor/peripheral demos all render through this so they look alike — no LVGL.
/// Body lines that fall past the bottom of the screen are simply clipped.
///
/// `strip_buf` must be in internal RAM (it is the SPI/DMA source — the ESP32
/// cannot DMA from PSRAM) and at least [`STRIP_BYTES`] long.
pub async fn draw_panel<DI, RST: OutputPin>(
    display: &mut Display<DI, lcd_async::models::ILI9342CRgb565, RST>,
    strip_buf: &mut [u8],
    board: &str,
    title: &str,
    body: &[&str],
) where
    DI: lcd_async::interface::Interface<Word = u8>,
{
    let header = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::CYAN);
    let white = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE);
    let rule = PrimitiveStyleBuilder::new()
        .fill_color(Rgb565::CYAN)
        .build();
    // FONT_9X18_BOLD is 9 px wide; right-align the title against the panel edge.
    let title_x = (W as i32 - title.len() as i32 * 9 - 8).max(8);
    for strip in 0..(H / STRIP_H) {
        let y_offset = (strip * STRIP_H) as i32;
        {
            let mut fb = RawFrameBuf::<Rgb565, _>::new(&mut strip_buf[..], W, STRIP_H);
            fb.clear(Rgb565::new(0, 0, 4)).ok();
            // Header row (board left, title right) + underline.
            Text::new(board, Point::new(8, 18 - y_offset), header)
                .draw(&mut fb)
                .ok();
            Text::new(title, Point::new(title_x, 18 - y_offset), header)
                .draw(&mut fb)
                .ok();
            Rectangle::new(Point::new(8, 26 - y_offset), Size::new(W as u32 - 16, 1))
                .into_styled(rule)
                .draw(&mut fb)
                .ok();
            // Body lines start below the header rule.
            for (i, line) in body.iter().enumerate() {
                let y = 44 + i as i32 * 18;
                Text::new(line, Point::new(8, y - y_offset), white)
                    .draw(&mut fb)
                    .ok();
            }
        }
        display
            .show_raw_data(
                0,
                (strip * STRIP_H) as u16,
                W as u16,
                STRIP_H as u16,
                strip_buf,
            )
            .await
            .ok();
    }
}

/// Probe I2C addresses `0x08..=0x77` and return the addresses that ACKed.
///
/// Takes the locked bus directly (`&mut I`); the caller prints the result
/// because the two boards log differently (RTT vs `log`). The bins pass the
/// locked `SharedI2cBus` guard, which derefs to an `embedded_hal_async` I2C.
pub async fn i2c_scan<I: embedded_hal_async::i2c::I2c>(bus: &mut I) -> heapless::Vec<u8, 32> {
    let mut found = heapless::Vec::new();
    for addr in 0x08u8..=0x77 {
        let mut buf = [0u8; 1];
        if bus.write_read(addr, &[], &mut buf).await.is_ok() {
            found.push(addr).ok();
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::wheel;

    #[test]
    fn wheel_primary_anchors() {
        assert_eq!(wheel(0), (255, 0, 0));
        assert_eq!(wheel(85), (0, 255, 0));
        assert_eq!(wheel(170), (0, 0, 255));
    }

    #[test]
    fn wheel_is_total() {
        // No input in 0..=255 may panic (overflow) — the wheel must be total.
        for pos in 0u8..=255 {
            let _ = wheel(pos);
        }
    }
}
