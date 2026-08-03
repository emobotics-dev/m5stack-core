// SPDX-License-Identifier: MIT OR Apache-2.0
//! **The LVGL threading pattern, and nothing else** — the version to copy (#63).
//!
//! A render thread, a flush thread, and an application task that must not be
//! delayed by either. `lvgl_sched.rs` measures this same pipeline under load;
//! this one exists to be read.
//!
//! # Converting an existing application
//!
//! The [`View`] below is ordinary oxivgl code and knows nothing about threads —
//! that is the point. Adopting this is a change to `main` only:
//!
//! 1. `set_flush_sync(SemaphoreFlushSync::leak_thread())` — block the flush wait
//!    in the scheduler. The default parks the core in `waiti 0` for the whole
//!    15-30 ms panel transfer, during which *nothing runs but ISRs*.
//! 2. [`sched::raise_app_executor`] — `#[esp_rtos::main]` starts at priority
//!    **zero, the lowest**, so the UI threads would otherwise outrank the work
//!    they exist to yield to. Skip this and the change makes latency *worse*.
//! 3. Spawn the render loop and the flush on threads, not on the shared
//!    executor and not on an `InterruptExecutor` — an interrupt executor makes
//!    the UI preempt everything, which is backwards.
//!
//! A `run_app(…)` call becomes [`Ui::init`] on the render thread followed by
//! [`Ui::run`]; nothing else about the view changes. Ladder: app 3 > flush 2 >
//! render 1, all inside 1..=3 so esp-radio's blob threads can never be starved
//! by the UI.
//!
//! Costs and design rules: `docs/lvgl-ui-performance.md`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

use core::ffi::c_void;

use demos::ui::{LVGL_BUF_BYTES, SCREEN_H, SCREEN_W, sched};
use demos::board;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use oxivgl::display::LvglBuffers;
use oxivgl::flush_pipeline::{SemaphoreFlushSync, flush_frame_buffer, set_flush_sync};
use oxivgl::view::{NavAction, RenderConfig, Ui, View};
use oxivgl::widgets::{Align, Arc, Label, Obj, WidgetError};
use static_cell::make_static;

m5stack_core::app_desc!();

/// Frame-rate target. The stock 32 ms refresh period caps the rate at 31 fps
/// *before* any render cost, so set it deliberately rather than inheriting it.
const TARGET_FPS: u32 = 30;

/// Carries the display from `main` to the flush thread — a thread entry is an
/// `extern "C" fn` and cannot capture.
static DISPLAY: Channel<CriticalSectionRawMutex, demos::ui::DisplayDriver, 1> = Channel::new();

/// Route LVGL assertions through the Rust panic handler (#57).
#[unsafe(no_mangle)]
pub extern "C" fn demos_lv_assert_handler() {
    panic!("LVGL assertion failed — see the LV_LOG_ERROR line above");
}

// --- the UI: ordinary oxivgl, unchanged by any of the above ----------------

/// One sweeping arc, so a stalled UI is visible without reading a log.
///
/// Every widget handle is kept, including the static label: a handle owns its
/// LVGL object and deletes it on drop, so a widget built and not stored simply
/// never appears — silently, with no error to notice.
#[derive(Default)]
struct Sweep {
    arc: Option<Arc<'static>>,
    label: Option<Label<'static>>,
    value: i32,
    dir: i32,
}

impl View for Sweep {
    fn create(&mut self, container: &Obj<'static>) -> Result<(), WidgetError> {
        let arc = Arc::new(container)?;
        arc.size(150, 150).align(Align::Center, 0, 0);
        arc.set_range_raw(0, 100);
        let label = Label::new(container)?;
        label.text("threaded").align(Align::BottomMid, 0, -8);
        self.arc = Some(arc);
        self.label = Some(label);
        self.dir = 1;
        Ok(())
    }

    fn update(&mut self) -> Result<NavAction, WidgetError> {
        self.value += self.dir * 3;
        if self.value >= 100 {
            self.value = 100;
            self.dir = -1;
        } else if self.value <= 0 {
            self.value = 0;
            self.dir = 1;
        }
        if let Some(arc) = &self.arc {
            arc.set_value_raw(self.value);
        }
        Ok(NavAction::None)
    }
}

/// Stand-in for the work that must not be delayed by the UI. In a real
/// application this is the protocol, the control loop, the radio.
#[embassy_executor::task]
async fn app_task() -> ! {
    let mut ticks = 0u32;
    loop {
        Timer::after(Duration::from_millis(10)).await;
        ticks += 1;
        if ticks % 500 == 0 {
            log::info!("app task alive: {} ticks", ticks);
        }
    }
}

// --- thread entries: each runs an executor and never returns ---------------

#[embassy_executor::task]
async fn render_task() -> ! {
    static mut BUFS: LvglBuffers<{ LVGL_BUF_BYTES }> = LvglBuffers::new();
    // SAFETY: touched only here, before this thread takes sole ownership of
    // LVGL for the rest of the program.
    let bufs = unsafe { &mut *core::ptr::addr_of_mut!(BUFS) };

    // `Ui::init` must run on this thread: every later LVGL call happens here.
    Ui::init(SCREEN_W.into(), SCREEN_H.into(), bufs)
        .run(Sweep::default(), RenderConfig::default().with_target_fps(TARGET_FPS))
        .await
}

#[embassy_executor::task]
async fn flush_task() -> ! {
    flush_frame_buffer(DISPLAY.receive().await).await
}

extern "C" fn render_thread(_: *mut c_void) {
    let exec = make_static!(esp_rtos::embassy::Executor::new());
    exec.run(|s| s.spawn(render_task().expect("spawn render")))
}

extern "C" fn flush_thread(_: *mut c_void) {
    let exec = make_static!(esp_rtos::embassy::Executor::new());
    exec.run(|s| s.spawn(flush_task().expect("spawn flush")))
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    demos::boot!(spawner, board, Lvgl);

    let (dma_rx, dma_tx) = demos::ui::dma_bufs();
    #[cfg(feature = "fire27")]
    let (dbus, _input) = board::lvgl_bringup(board.spi2, board.buttons, dma_rx, dma_tx).await;
    #[cfg(feature = "cores3")]
    let (dbus, _i2c) = board::lvgl_bringup(board.spi2, board.i2c0, dma_rx, dma_tx).await;
    DISPLAY.try_send(demos::ui::DisplayDriver::new(dbus)).ok();

    // (1) BEFORE the display is created — the registration is read by both the
    // LVGL wait callback and the flush task. `leak_thread` because the flush
    // below is a thread; an interrupt executor needs `leak_isr` instead.
    set_flush_sync(SemaphoreFlushSync::leak_thread());

    // (2) BEFORE the UI threads exist. Without it they outrank the app task.
    sched::raise_app_executor();

    // (3) Threads, at the flush > render rungs of the ladder.
    // SAFETY: both entries run an executor and never return.
    unsafe {
        sched::spawn("ui-flush", flush_thread, sched::PRIO_FLUSH, sched::FLUSH_STACK, 0);
        sched::spawn(
            "ui-render",
            render_thread,
            sched::PRIO_RENDER,
            sched::RENDER_STACK,
            sched::RENDER_CORE,
        );
    }

    spawner.spawn(app_task().expect("spawn app task"));

    // The app executor stays free for application work.
    core::future::pending::<()>().await
}
