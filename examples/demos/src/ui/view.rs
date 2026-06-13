// SPDX-License-Identifier: MIT OR Apache-2.0
//! The demo screen: a title, a centered animated [`Spinner`], and a frame
//! counter that increments on every `update()` so the refresh is visible.

use oxivgl::{
    style::{Selector, Style},
    view::{NavAction, View},
    widgets::{Align, Label, Obj, Spinner, WidgetError},
};

/// Widgets are owned by the struct because LVGL deletes the underlying objects
/// when the wrapper is dropped — keeping them here keeps them alive.
#[derive(Default)]
pub struct DemoView {
    /// Frame counter label; its text is rewritten each `update()`.
    counter_label: Option<Label<'static>>,
    /// Kept alive for the program's lifetime (LVGL deletes on Drop).
    _title: Option<Label<'static>>,
    /// Kept alive for the program's lifetime (LVGL deletes on Drop).
    _spinner: Option<Spinner<'static>>,
    /// Frames rendered so far.
    frame: u32,
    /// `true` once the keypad indev has been registered (Fire27 only).
    #[cfg(feature = "fire27")]
    indev_registered: bool,
}

impl View for DemoView {
    fn create(&mut self, container: &Obj<'static>) -> Result<(), WidgetError> {
        // The CoreS3 is touch-only and registers no keypad indev.
        #[cfg(feature = "fire27")]
        if !self.indev_registered {
            crate::ui::input::register_keypad_indev();
            self.indev_registered = true;
        }

        let bg = Style::new(|s| {
            s.bg_color_hex(0x101820).bg_opa(255).text_color_hex(0xffffff);
        });
        container.add_style(&bg, Selector::DEFAULT);

        let title = Label::new(container)?;
        title
            // ASCII only: LVGL's built-in Montserrat font omits non-ASCII
            // glyphs (e.g. U+00B7 "·"), which render as a missing-glyph box.
            .text("m5stack-core - oxivgl")
            .align(Align::TopMid, 0, 12);

        let spinner = Spinner::new(container)?;
        spinner.size(90, 90).align(Align::Center, 0, -10);
        spinner.set_anim_params(1000, 200);

        let counter = Label::new(container)?;
        counter.text("frame: 0").align(Align::BottomMid, 0, -16);

        self._title = Some(title);
        self._spinner = Some(spinner);
        self.counter_label = Some(counter);
        Ok(())
    }

    fn update(&mut self) -> Result<NavAction, WidgetError> {
        self.frame = self.frame.wrapping_add(1);
        if let Some(label) = &self.counter_label {
            let mut buf = heapless::String::<24>::new();
            // Ignore formatting errors: the buffer is large enough for the
            // text, and a transient miss only skips one counter repaint.
            let _ = core::fmt::Write::write_fmt(&mut buf, format_args!("frame: {}", self.frame));
            label.text(&buf);
        }
        Ok(NavAction::None)
    }
}
