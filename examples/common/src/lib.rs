// SPDX-License-Identifier: MIT OR Apache-2.0
//! Chip-agnostic helpers shared by the per-board BSP examples.
//!
//! This crate deliberately depends on neither `m5stack-core` nor any esp-hal
//! crate and takes no chip feature: it holds only the pure-`no_std`, reusable
//! pieces (the colour wheel, the strip-rendered display routines, the I2C bus
//! scan, and the shared display geometry constants). Per-board chip-specific
//! bring-up lives in each example's own `lib.rs`.
#![no_std]

use embedded_graphics::{
    draw_target::DrawTarget,
    mono_font::{MonoTextStyle, ascii::FONT_9X18_BOLD},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, PrimitiveStyleBuilder, Rectangle, Triangle},
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

/// Draw the demo scene into a `DrawTarget`, offsetting every coordinate by `y`
/// so a single full-screen scene can be rendered one strip at a time.
fn draw_demo_strip(fb: &mut impl DrawTarget<Color = Rgb565>, board: &str, footer: &[&str], y: i32) {
    let white = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::WHITE);
    let gray = MonoTextStyle::new(&FONT_9X18_BOLD, Rgb565::CSS_LIGHT_GRAY);

    // title
    Text::new("m5stack-core BSP", Point::new(70, 30 - y), white)
        .draw(fb)
        .ok();

    // yellow rectangle with board name
    let rect = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb565::YELLOW)
        .stroke_width(2)
        .fill_color(Rgb565::new(4, 8, 0))
        .build();
    Rectangle::new(Point::new(20, 50 - y), Size::new(120, 80))
        .into_styled(rect)
        .draw(fb)
        .ok();
    Text::new(board, Point::new(45, 95 - y), white)
        .draw(fb)
        .ok();

    // cyan circle
    let circle = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb565::CYAN)
        .stroke_width(2)
        .fill_color(Rgb565::new(0, 8, 4))
        .build();
    Circle::new(Point::new(170, 55 - y), 70)
        .into_styled(circle)
        .draw(fb)
        .ok();

    // green triangle
    let green = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb565::GREEN)
        .stroke_width(2)
        .fill_color(Rgb565::new(0, 12, 0))
        .build();
    Triangle::new(
        Point::new(100, 160 - y),
        Point::new(40, 230 - y),
        Point::new(160, 230 - y),
    )
    .into_styled(green)
    .draw(fb)
    .ok();

    // red triangle
    let red = PrimitiveStyleBuilder::new()
        .stroke_color(Rgb565::RED)
        .stroke_width(2)
        .fill_color(Rgb565::new(8, 0, 0))
        .build();
    Triangle::new(
        Point::new(250, 150 - y),
        Point::new(190, 230 - y),
        Point::new(310, 230 - y),
    )
    .into_styled(red)
    .draw(fb)
    .ok();

    // footer labels evenly spaced
    let spacing = W as i32 / (footer.len() as i32 + 1);
    for (i, label) in footer.iter().enumerate() {
        let x = spacing * (i as i32 + 1) - (label.len() as i32 * 9 / 2);
        Text::new(label, Point::new(x, 235 - y), gray).draw(fb).ok();
    }
}

/// Render the demo splash (title, board-name rect, circle, two triangles,
/// footer) using a caller-provided strip framebuffer.
///
/// `strip_buf` must be in internal RAM (it is the SPI/DMA source — the ESP32
/// cannot DMA from PSRAM) and at least [`STRIP_BYTES`] long.
pub async fn draw_demo<DI, RST: OutputPin>(
    display: &mut Display<DI, lcd_async::models::ILI9342CRgb565, RST>,
    strip_buf: &mut [u8],
    board: &str,
    footer: &[&str],
) where
    DI: lcd_async::interface::Interface<Word = u8>,
{
    for strip in 0..(H / STRIP_H) {
        let y_offset = strip * STRIP_H;
        {
            let mut fb = RawFrameBuf::<Rgb565, _>::new(&mut strip_buf[..], W, STRIP_H);
            fb.clear(Rgb565::new(0, 0, 4)).ok();
            draw_demo_strip(&mut fb, board, footer, y_offset as i32);
        }
        display
            .show_raw_data(0, y_offset as u16, W as u16, STRIP_H as u16, strip_buf)
            .await
            .ok();
    }
}

/// Render a list of text lines full-screen using the reused strip framebuffer.
///
/// `strip_buf` has the same internal-RAM requirement as in [`draw_demo`].
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
/// `strip_buf` has the same internal-RAM requirement as in [`draw_demo`].
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
    let rule = PrimitiveStyleBuilder::new().fill_color(Rgb565::CYAN).build();
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
            Rectangle::new(
                Point::new(8, 26 - y_offset),
                Size::new(W as u32 - 16, 1),
            )
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
            .show_raw_data(0, (strip * STRIP_H) as u16, W as u16, STRIP_H as u16, strip_buf)
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
