// SPDX-License-Identifier: MIT OR Apache-2.0
//! **The LVGL threading pattern, and nothing else** — the version to copy (#63).
//!
//! A render thread, a flush thread, and an application task that must not be
//! delayed by either. `lvgl_sched.rs` measures this same pipeline under load;
//! this one exists to be read.
//!
//! Three things carry it, and all three are easy to get subtly wrong:
//!
//! 1. **Threads, not another `InterruptExecutor`.** An interrupt executor makes
//!    the UI preempt *everything*, which is backwards — latency-sensitive work
//!    has to win.
//! 2. **Raise the app executor.** `#[esp_rtos::main]` starts at priority
//!    **zero, the lowest**, so a render thread at any priority outranks the work
//!    it exists to yield to. Skip this line and the change makes things *worse*.
//! 3. **Block on a semaphore, not `waiti`.** `demos::ui::pipeline` registers its
//!    own LVGL flush callbacks for this: the render thread leaves the run queue
//!    for the whole transfer instead of parking the core. LVGL still stays
//!    single-threaded — the flush side only moves bytes, and
//!    `lv_display_flush_ready` is called from the render thread — so
//!    `LV_USE_OS LV_OS_NONE` remains correct.
//!
//! Ladder: app 3 > flush 2 > render 1, all inside 1..=3 so esp-radio's blob
//! threads (far above) can never be starved by the UI.
//!
//! Costs and design rules: `docs/lvgl-ui-performance.md`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

use core::ffi::c_void;

use demos::ui::{LVGL_BUF_BYTES, SCREEN_H, SCREEN_W, pipeline, sched};
use demos::{board, shim};
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use oxivgl::display::LvglBuffers;
use oxivgl::widgets::{Align, Arc, Label, Screen};
use static_cell::make_static;

m5stack_core::app_desc!();

/// Refresh period. The stock 32 ms caps the frame rate at 31 fps *before* any
/// render cost, so set it deliberately rather than inheriting it.
const REFRESH_MS: u32 = 32;

/// Carries the display from `main` to the flush thread.
static DISPLAY: Channel<CriticalSectionRawMutex, demos::ui::DisplayDriver, 1> = Channel::new();

/// Route LVGL assertions through the Rust panic handler (#57).
#[unsafe(no_mangle)]
pub extern "C" fn demos_lv_assert_handler() {
    panic!("LVGL assertion failed — see the LV_LOG_ERROR line above");
}

// --- the UI: one sweeping arc, so there is something to look at ------------

#[embassy_executor::task]
async fn render_task() -> ! {
    static mut BUFS: LvglBuffers<{ LVGL_BUF_BYTES }> = LvglBuffers::new();
    // SAFETY: touched only here, before this thread takes sole ownership of
    // LVGL for the rest of the program.
    let bufs = unsafe { &mut *core::ptr::addr_of_mut!(BUFS) };

    // SAFETY: first and only LVGL use, on the thread that keeps it.
    let driver = unsafe { pipeline::init(SCREEN_W.into(), SCREEN_H.into(), bufs) };
    pipeline::set_refresh_period(REFRESH_MS);
    pipeline::READY.wait().await;

    let screen = Screen::active().expect("no active screen");
    screen.bg_color(0x0d1117).bg_opa(255).text_color(0xffffff);
    let arc = Arc::new(&screen).expect("arc");
    arc.size(150, 150).align(Align::Center, 0, 0);
    arc.set_range_raw(0, 100);
    let label = Label::new(&screen).expect("label");
    label.text("threaded").align(Align::BottomMid, 0, -8);

    let (mut value, mut dir) = (0i32, 1i32);
    let mut last = Instant::now();
    loop {
        let now = Instant::now();
        let dt = (now - last).as_millis() as i32;
        last = now;

        value += dir * (100 * dt) / 1000;
        if value >= 100 {
            value = 100;
            dir = -1;
        } else if value <= 0 {
            value = 0;
            dir = 1;
        }
        arc.set_value_raw(value);

        // LVGL says when it next wants attention; honour it rather than spin.
        let delay = driver.timer_handler();
        Timer::after(Duration::from_millis(delay.clamp(1, 10) as u64)).await;
    }
}

#[embassy_executor::task]
async fn flush_task() -> ! {
    let display = DISPLAY.receive().await;
    pipeline::flush_worker(display).await
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
    let p = board::init();
    let board = board::Board::split(p);
    shim::init_heap_lvgl();
    esp_rtos::start(board.system.timer0_0, board.system.sw_int.software_interrupt0);
    #[cfg(feature = "fire27")]
    let _console = shim::init_console(
        spawner,
        board::console_serial(board.uart0, board.uart0_rx, board.uart0_tx),
    );
    #[cfg(feature = "cores3")]
    let _console = shim::init_console(spawner, board::console_serial(board.usb_device));

    let (dma_rx, dma_tx) = demos::ui::dma_bufs();
    #[cfg(feature = "fire27")]
    let (dbus, _input) = board::lvgl_bringup(board.spi2, board.buttons, dma_rx, dma_tx).await;
    #[cfg(feature = "cores3")]
    let (dbus, _i2c) = board::lvgl_bringup(board.spi2, board.i2c0, dma_rx, dma_tx).await;
    DISPLAY.try_send(demos::ui::DisplayDriver::new(dbus)).ok();

    // (2) BEFORE the UI threads exist. Without it they outrank the app task.
    sched::raise_app_executor();

    // (1) Threads, at the flush > render rungs of the ladder.
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
