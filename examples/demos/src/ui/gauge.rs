// SPDX-License-Identifier: MIT OR Apache-2.0
//! The redraw load: a sweeping meter gauge.
//!
//! Deliberately not a pretty UI. It exists to dirty a large, *constant* area
//! every frame so the frame rate is a measurement rather than an impression, and
//! to be unmistakable on camera — a needle that stops moving is a failure you can
//! see without reading a log.

use heapless::String;
use oxivgl::style::{Selector, Style};
use oxivgl::widgets::{Align, Arc, AsLvHandle, Bar, Label, Screen, WidgetError};

/// Sweep rate in gauge units per second. One full 0-100-0 cycle takes ~2 s.
const SWEEP_PER_S: i32 = 100;

/// Which widgets animate. Cycled at runtime to attribute render cost: comparing
/// cycles-per-pixel across these separates a per-pixel cost from a fixed
/// per-frame one, which raw totals cannot.
#[derive(Clone, Copy, PartialEq)]
pub enum Load {
    /// Nothing invalidates — whatever `lv_timer_handler` still costs is floor.
    Idle,
    /// A plain filled rectangle: no anti-aliasing, no mask.
    Bar,
    /// Text only: glyph blits.
    Text,
    /// Anti-aliased arc, small radius.
    ArcSmall,
    /// The same arc, large radius — same widget, ~4x the pixels.
    ArcLarge,
    /// Whole screen invalidated every frame: the many-chunk case, where the
    /// per-chunk cost is paid as often as the buffer forces.
    FullScreen,
    /// Eight small bars animating together. The one profile that varies the
    /// thing the cost model says dominates — object *count* — while holding
    /// pixels roughly constant.
    ManyObjects,
}

impl Load {
    pub const ALL: [Load; 7] = [
        Load::Idle,
        Load::Bar,
        Load::Text,
        Load::ArcSmall,
        Load::ArcLarge,
        Load::FullScreen,
        Load::ManyObjects,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Load::Idle => "idle",
            Load::Bar => "bar",
            Load::Text => "text",
            Load::ArcSmall => "arc-small",
            Load::ArcLarge => "arc-large",
            Load::FullScreen => "fullscreen",
            Load::ManyObjects => "many-objects",
        }
    }
}

pub struct Gauge {
    screen: Screen,
    many: heapless::Vec<Bar<'static>, 8>,
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

        // Eight independent bars for the object-count profile. Untouched by the
        // other profiles, and an untouched widget is never invalidated, so they
        // cost nothing when idle.
        let mut many: heapless::Vec<Bar<'static>, 8> = heapless::Vec::new();
        for i in 0..8 {
            let bar = Bar::new(&screen)?;
            bar.size(28, 10).align(Align::TopLeft, 6 + (i as i32) * 38, 24);
            bar.set_range_raw(0, 100);
            bar.set_value_raw(0, false);
            let _ = many.push(bar);
        }

        Ok(Self { screen, many, arc, bar, readout, stats, value: 0, dir: 1 })
    }

    /// Resize the arc for the current profile. Only called on a profile change,
    /// so the one-off full invalidate it causes never lands inside a sample.
    pub fn set_load(&mut self, load: Load) {
        match load {
            Load::ArcSmall => {
                self.arc.size(70, 70);
            }
            Load::ArcLarge => {
                self.arc.size(150, 150);
            }
            _ => {}
        }
    }

    /// Advance the sweep by wall-clock time, so the animation speed does not
    /// depend on how often the render loop happens to run. Only the widgets the
    /// profile names are touched — an untouched widget is never invalidated, so
    /// it costs nothing to draw.
    pub fn step(&mut self, dt_ms: u32, load: Load) {
        let delta = (SWEEP_PER_S * dt_ms as i32) / 1000;
        if delta == 0 || load == Load::Idle {
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
        match load {
            Load::Bar => {
                self.bar.set_value_raw(self.value, false);
            }
            Load::Text => {
                let mut s: String<8> = String::new();
                let _ = core::fmt::Write::write_fmt(&mut s, format_args!("{}", self.value));
                self.readout.text(&s);
            }
            Load::ArcSmall | Load::ArcLarge => {
                self.arc.set_value_raw(self.value);
            }
            Load::ManyObjects => {
                // Each bar gets a different phase so all eight really change.
                for (i, b) in self.many.iter().enumerate() {
                    let v = (self.value + (i as i32) * 12) % 101;
                    b.set_value_raw(v, false);
                }
            }
            Load::FullScreen => {
                self.bar.set_value_raw(self.value, false);
                // SAFETY: called on the render thread, the only LVGL caller.
                unsafe { oxivgl_sys::lv_obj_invalidate(self.screen.lv_handle()) };
            }
            Load::Idle => {}
        }
    }

    /// Mirror the measured rate on-screen so a camera frame carries the number.
    /// Skipped while profiling: this label is itself a per-frame redraw, and it
    /// would land in whichever profile happened to be running.
    pub fn show_stats(&mut self, fps: u32, worst_latency_ms: u32) {
        let mut s: String<32> = String::new();
        let _ = core::fmt::Write::write_fmt(
            &mut s,
            format_args!("fps {fps}  lat {worst_latency_ms}ms"),
        );
        self.stats.text(&s);
    }
}
