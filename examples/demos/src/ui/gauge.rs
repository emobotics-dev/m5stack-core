// SPDX-License-Identifier: MIT OR Apache-2.0
//! The redraw load: a sweeping meter gauge.
//!
//! Deliberately not a pretty UI. It exists to dirty a large, *constant* area
//! every frame so the frame rate is a measurement rather than an impression, and
//! to be unmistakable on camera — a needle that stops moving is a failure you can
//! see without reading a log.

use heapless::String;
use oxivgl::style::{Selector, Style};
use oxivgl::widgets::{Align, Arc, Bar, Label, Screen, WidgetError};

/// Sweep rate in gauge units per second. One full 0-100-0 cycle takes ~2 s.
const SWEEP_PER_S: i32 = 100;

pub struct Gauge {
    arc: Arc<'static>,
    bar: Bar<'static>,
    readout: Label<'static>,
    stats: Label<'static>,
    value: i32,
    dir: i32,
}

impl Gauge {
    pub fn new(mode: &str) -> Result<Self, WidgetError> {
        let screen = Screen::active().expect("no active screen");
        let bg = Style::new(|s| {
            s.bg_color_hex(0x0d1117).bg_opa(255).text_color_hex(0xffffff);
        });
        screen.add_style(&bg, Selector::DEFAULT);

        let title = Label::new(&screen)?;
        let mut t: String<32> = String::new();
        let _ = core::fmt::Write::write_fmt(&mut t, format_args!("sched: {mode}"));
        title.text(&t).align(Align::TopMid, 0, 4);

        let arc = Arc::new(&screen)?;
        arc.size(150, 150).align(Align::Center, 0, -6);
        arc.set_range_raw(0, 100);
        arc.set_bg_angles(135, 45);
        arc.set_value_raw(0);

        let readout = Label::new(&screen)?;
        readout.text("0").align(Align::Center, 0, -6);

        let bar = Bar::new(&screen)?;
        bar.size(280, 12).align(Align::BottomMid, 0, -26);
        bar.set_range_raw(0, 100);
        bar.set_value_raw(0, false);

        let stats = Label::new(&screen)?;
        stats.text("fps --").align(Align::BottomMid, 0, -6);

        Ok(Self { arc, bar, readout, stats, value: 0, dir: 1 })
    }

    /// Advance the sweep by wall-clock time, so the animation speed does not
    /// depend on how often the render loop happens to run.
    pub fn step(&mut self, dt_ms: u32) {
        let delta = (SWEEP_PER_S * dt_ms as i32) / 1000;
        if delta == 0 {
            return;
        }
        self.value += self.dir * delta;
        if self.value >= 100 {
            self.value = 100;
            self.dir = -1;
        } else if self.value <= 0 {
            self.value = 0;
            self.dir = 1;
        }
        self.arc.set_value_raw(self.value);
        self.bar.set_value_raw(self.value, false);
        let mut s: String<8> = String::new();
        let _ = core::fmt::Write::write_fmt(&mut s, format_args!("{}", self.value));
        self.readout.text(&s);
    }

    /// Mirror the measured rate on-screen so a camera frame carries the number.
    pub fn show_stats(&mut self, fps: u32, worst_latency_ms: u32) {
        let mut s: String<32> = String::new();
        let _ = core::fmt::Write::write_fmt(
            &mut s,
            format_args!("fps {fps}  lat {worst_latency_ms}ms"),
        );
        self.stats.text(&s);
    }
}
