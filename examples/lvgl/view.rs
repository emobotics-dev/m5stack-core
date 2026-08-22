// SPDX-License-Identifier: MIT OR Apache-2.0
//! Interactive demo screen: three focusable LVGL buttons navigated by the
//! front-panel input. `PREV`/`NEXT` move focus, `ENTER` clicks the focused
//! button (incrementing a counter). A frame counter shows the render loop is
//! live. Same view on both boards — only the input *source* differs (Fire27
//! buttons vs CoreS3 touch), and that's hidden behind the keypad indev.

use oxivgl::{
    enums::EventCode,
    event::Event,
    group::{Group, GroupRef},
    style::{Selector, Style},
    view::{NavAction, View},
    widgets::{Align, Button, Label, Obj, WidgetError},
};

const NAMES: [&str; 3] = ["A", "B", "C"];

#[derive(Default)]
pub struct MenuView {
    /// Focus group holding the three buttons (returned via `input_group`).
    group: Option<Group>,
    /// The buttons, kept for `on_event` click matching (and alive for LVGL).
    buttons: [Option<Button<'static>>; 3],
    /// Status line — updated on each click.
    status: Option<Label<'static>>,
    /// Frame counter — updated every `update()` tick.
    frame: Option<Label<'static>>,
    clicks: u32,
    tick: u32,
    /// CoreS3: the touchscreen POINTER indev, registered in `create` and kept
    /// alive for the view's lifetime (Fire27 uses the keypad indev instead).
    #[cfg(feature = "cores3")]
    _pointer: Option<oxivgl::indev::PointerIndev>,
}

impl View for MenuView {
    fn create(&mut self, container: &Obj<'static>) -> Result<(), WidgetError> {
        let bg = Style::new(|s| {
            s.bg_color_hex(0x101820)
                .bg_opa(255)
                .text_color_hex(0xffffff);
        });
        container.add_style(&bg, Selector::DEFAULT);

        Label::new(container)?
            .text("m5stack-core - input")
            .align(Align::TopMid, 0, 8);

        // Three focusable buttons in a row, each in the focus group.
        let group = Group::new()?;
        for (i, name) in NAMES.iter().enumerate() {
            let btn = Button::new(container)?;
            btn.size(64, 48)
                .align(Align::Center, (i as i32 - 1) * 84, -8);
            Label::new(&btn)?.text(name).align(Align::Center, 0, 0);
            group.add_obj(&btn);
            self.buttons[i] = Some(btn);
        }

        let status = Label::new(container)?;
        status
            .text("PREV/NEXT focus - ENTER select")
            .align(Align::BottomMid, 0, -28);

        let frame = Label::new(container)?;
        frame.text("frame: 0").align(Align::BottomMid, 0, -8);

        self.status = Some(status);
        self.frame = Some(frame);
        self.group = Some(group);

        // CoreS3: register a touchscreen POINTER indev so the buttons are
        // tapped directly by coordinate (fed by `input::touch_poll_task`).
        // Fire27 drives the same buttons via the keypad indev instead.
        #[cfg(feature = "cores3")]
        {
            self._pointer = Some(oxivgl::indev::PointerIndev::new(
                &crate::ui::input::POINTER,
            )?);
        }
        Ok(())
    }

    fn on_event(&mut self, event: &Event) -> NavAction {
        for (i, btn) in self.buttons.iter().enumerate() {
            if let Some(btn) = btn {
                if event.matches(btn, EventCode::CLICKED) {
                    self.clicks = self.clicks.wrapping_add(1);
                    log::info!("button {} clicked ({})", NAMES[i], self.clicks);
                    if let Some(status) = &self.status {
                        let mut buf = heapless::String::<32>::new();
                        let _ = core::fmt::Write::write_fmt(
                            &mut buf,
                            format_args!("{} clicked ({})", NAMES[i], self.clicks),
                        );
                        status.text(&buf);
                    }
                }
            }
        }
        NavAction::None
    }

    fn update(&mut self) -> Result<NavAction, WidgetError> {
        self.tick = self.tick.wrapping_add(1);
        if let Some(frame) = &self.frame {
            let mut buf = heapless::String::<24>::new();
            let _ = core::fmt::Write::write_fmt(&mut buf, format_args!("frame: {}", self.tick));
            frame.text(&buf);
        }
        Ok(NavAction::None)
    }

    fn input_group(&self) -> Option<GroupRef> {
        self.group.as_ref().map(|g| g.as_ref())
    }
}
