// SPDX-License-Identifier: MIT OR Apache-2.0
//! LVGL render/flush **pipeline stress harness** — not a UI demo (#63).
//!
//! It loads the pipeline in known ways and reports what that costs. A sweeping
//! gauge supplies redraw load, a 10 ms-period probe task stands in for
//! latency-sensitive application work and records how late it was actually
//! woken, and load profiles cycle at runtime so every number comes from one
//! flash under identical conditions.
//!
//! The profiles each vary one thing — nothing, a plain fill, text, a small arc,
//! the same arc enlarged, a full-screen invalidate, and eight independent
//! objects — which is what makes the cost model measurable rather than
//! arguable. The last one matters most: cost tracks the number of draw tasks,
//! so eight bars cost ~7x one while drawing *fewer* pixels.
//!
//! Findings and the resulting design rules: `docs/lvgl-ui-performance.md`.
//!
//! Three modes, selected by feature so the build fingerprint changes and a stale
//! binary cannot be measured by mistake:
//!
//! | features | render | flush |
//! |---|---|---|
//! | *(none)* | shared app executor | interrupt executor |
//! | `ui-thread` | own thread, prio 1 | interrupt executor |
//! | `ui-thread,ui-flush-thread` | own thread, prio 1 | own thread, prio 2 |
//!
//! The first is today's `lvgl.rs` pattern. The other two also raise the app
//! executor to priority 3, without which the UI threads would outrank the
//! latency-sensitive work they exist to yield to.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[cfg(feature = "ui-thread")]
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use demos::board;
use demos::shim;
use demos::ui::gauge::{Gauge, Load};
use demos::ui::{LVGL_BUF_BYTES, SCREEN_H, SCREEN_W, metrics, pipeline};
#[cfg(feature = "ui-thread")]
use demos::ui::sched;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use oxivgl::display::LvglBuffers;
use static_cell::make_static;

m5stack_core::app_desc!();

// Reproducible hang on ESP32: the render loop makes exactly one
// `lv_timer_handler` call and then blocks in `wait_cb` — the flush never
// completes with render on APP and flush on PRO. Works on ESP32-S3. Not
// diagnosed; a build error beats a silent freeze.
#[cfg(all(feature = "ui-app-core", feature = "fire27"))]
compile_error!(
    "ui-app-core hangs on fire27: render on APP + flush on PRO never completes a \
     flush (confirmed twice on hardware). Use ui-flush-thread on ESP32."
);

#[cfg(all(feature = "ui-flush-thread", not(feature = "ui-thread")))]
compile_error!(
    "ui-flush-thread requires ui-thread: a flush thread above a render loop that \
     still shares the app executor measures nothing #63 is about"
);

#[cfg(feature = "ui-app-core")]
const MODE: &str = "render on APP";
#[cfg(all(feature = "ui-thread", feature = "ui-flush-thread", not(feature = "ui-app-core")))]
const MODE: &str = "render+flush threads";
#[cfg(all(feature = "ui-thread", not(feature = "ui-flush-thread")))]
const MODE: &str = "render thread";
#[cfg(not(feature = "ui-thread"))]
const MODE: &str = "shared executor";

/// Probe period. Short enough that a single blocked flush shows up, long enough
/// not to be a load in itself.
const PROBE_MS: u64 = 10;
/// LVGL refresh period. The stock 32 ms caps the rate at 31 fps before any
/// render cost, which is below the 30 fps target once drawing is counted.
const REFRESH_MS: u32 = 32;

/// Carries the display from `main` to whichever context runs the flush.
static DRIVER: Channel<CriticalSectionRawMutex, demos::ui::DisplayDriver, 1> = Channel::new();
/// Which load profile is running, so the stats line can name it.
static PROFILE: AtomicUsize = AtomicUsize::new(0);
/// Seconds each profile runs. The first second after a switch is discarded when
/// reading results — the resize itself dirties the screen once.
const PROFILE_SECS: u64 = 6;

/// Last interval's `(fps, worst latency ms)`, for the on-screen readout.
static STATS: Signal<CriticalSectionRawMutex, (u32, u32)> = Signal::new();

/// Route LVGL assertions through the Rust panic handler instead of LVGL's
/// default `while(1);` (#57).
#[unsafe(no_mangle)]
pub extern "C" fn demos_lv_assert_handler() {
    panic!("LVGL assertion failed — see the LV_LOG_ERROR line above");
}

#[embassy_executor::task]
async fn flush_task() -> ! {
    let driver = DRIVER.receive().await;
    pipeline::flush_worker(driver).await
}

/// The latency-sensitive work the UI must not disturb: wake on a fixed period
/// and record how late it actually ran.
#[embassy_executor::task]
async fn latency_probe() -> ! {
    let period = Duration::from_millis(PROBE_MS);
    let mut next = Instant::now() + period;
    loop {
        Timer::at(next).await;
        let now = Instant::now();
        metrics::LATENCY.record((now - next).as_micros() as u32);
        next += period;
        // Resync rather than chase a backlog if we fell more than a period behind.
        if next < now {
            next = now + period;
        }
    }
}

#[embassy_executor::task]
async fn stats_task() -> ! {
    let mut bytes = 0u32;
    let mut ops = 0u32;
    // Prime the deltas so the first interval is not a partial one.
    metrics::take_frames(&mut bytes, &mut ops);
    metrics::LATENCY.take();
    metrics::IDLE_US[0].store(0, Relaxed);
    metrics::IDLE_US[1].store(0, Relaxed);
    loop {
        Timer::after(Duration::from_secs(1)).await;
        let (frames, d_bytes, d_ops) = metrics::take_frames(&mut bytes, &mut ops);
        let l = metrics::LATENCY.take();
        let busy = metrics::take_busy_pct(0, 1_000_000);
        let mut app_col: heapless::String<8> = heapless::String::new();
        #[cfg(feature = "ui-app-core")]
        let _ = core::fmt::Write::write_fmt(
            &mut app_col,
            format_args!("{}%", metrics::take_busy_pct(1, 1_000_000)),
        );
        #[cfg(not(feature = "ui-app-core"))]
        let _ = app_col.push_str("off");
        let handler = metrics::HANDLER_US.swap(0, Relaxed);
        let wait = metrics::WAIT_US.swap(0, Relaxed);
        let calls = metrics::HANDLER_CALLS.swap(0, Relaxed).max(1);
        let px = (d_bytes / 2).max(1);
        let cycles_px = (handler - wait) as u64 * 240 / px as u64;
        log::info!(
            "[{}/{}] fps={} {} pro={}% app={} draw={}% wait={}% {}px/s {}cyc/px calls={} us/call={} flush={}ops {}kB/s | probe n={} mean={}us max={}us >5ms={} >20ms={}",
            MODE,
            Load::ALL[PROFILE.load(Relaxed)].name(),
            frames,
            metrics::regime((handler - wait) / 10_000, wait / 10_000),
            busy,
            app_col,
            (handler - wait) / 10_000,
            wait / 10_000,
            px,
            cycles_px,
            calls,
            (handler - wait) / calls,
            d_ops,
            d_bytes / 1024,
            l.count,
            l.mean_us,
            l.max_us,
            l.over_5ms,
            l.over_20ms,
        );
        demos::ui::lvprof::report(1_000_000, 12);
        demos::ui::lvasm::report();
        STATS.signal((frames, l.max_us / 1000));
    }
}

/// The render loop. Owns every LVGL call, wherever this runs.
async fn render_loop() -> ! {
    static mut LVGL_BUFS: LvglBuffers<{ LVGL_BUF_BYTES }> = LvglBuffers::new();
    // SAFETY: touched only here, before the single-threaded render loop takes
    // ownership for the rest of the program.
    let bufs = unsafe { &mut *core::ptr::addr_of_mut!(LVGL_BUFS) };

    // SAFETY: first and only LVGL use, and this is the thread that keeps it.
    let driver = unsafe { pipeline::init(SCREEN_W.into(), SCREEN_H.into(), bufs) };
    pipeline::set_refresh_period(REFRESH_MS);
    pipeline::READY.wait().await;
    log::info!("display ready — mode: {}", MODE);

    let mut gauge = Gauge::new(MODE).expect("gauge create");
    let mut last = Instant::now();
    let mut profile_since = Instant::now();
    let mut profile = 0usize;
    gauge.set_load(Load::ALL[profile]);
    loop {
        let now = Instant::now();
        let dt = (now - last).as_millis() as u32;
        last = now;

        if (now - profile_since).as_secs() >= PROFILE_SECS {
            profile = (profile + 1) % Load::ALL.len();
            gauge.set_load(Load::ALL[profile]);
            PROFILE.store(profile, Relaxed);
            profile_since = now;
        }
        gauge.step(dt, Load::ALL[profile]);
        // The on-screen readout is deliberately not updated while profiling: it
        // is itself a per-frame redraw and would be charged to whichever profile
        // happened to be running.
        let _ = &STATS;

        // One frame == one refresh that actually flushed, however LVGL split it.
        let before = metrics::SUBMITS.load(Relaxed);
        let t0 = esp_radio_rtos_driver::now();
        let delay = driver.timer_handler();
        metrics::HANDLER_CALLS.fetch_add(1, Relaxed);
        metrics::HANDLER_US.fetch_add((esp_radio_rtos_driver::now() - t0) as u32, Relaxed);
        if metrics::SUBMITS.load(Relaxed) != before {
            metrics::FRAMES.fetch_add(1, Relaxed);
        }
        Timer::after(Duration::from_millis(delay.clamp(1, 10) as u64)).await;
    }
}

#[embassy_executor::task]
async fn render_task() -> ! {
    render_loop().await
}

#[cfg(feature = "ui-thread")]
extern "C" fn render_thread(_: *mut c_void) {
    let exec = make_static!(esp_rtos::embassy::Executor::new());
    exec.run(|s| s.spawn(render_task().expect("spawn render task")))
}

#[cfg(feature = "ui-flush-thread")]
extern "C" fn flush_thread(_: *mut c_void) {
    let exec = make_static!(esp_rtos::embassy::Executor::new());
    exec.run(|s| s.spawn(flush_task().expect("spawn flush task")))
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();
    let board = board::Board::split(p);
    // HeapProfile::Lvgl is enough here. Enabling LV_USE_OS is not: a software
    // draw unit allocates its own layer state and that 50 kB reclaimed-ROM pool
    // runs out (OOM at 51192 used, 8 free), so Default is needed for that.
    shim::init_heap_lvgl();
    esp_rtos::start_with_idle_hook(
        board.system.timer0_0,
        board.system.sw_int.software_interrupt0,
        metrics::idle_hook,
    );
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
    DRIVER.try_send(demos::ui::DisplayDriver::new(dbus)).ok();

    // Lift the app executor above the UI *before* the UI threads exist. Without
    // this the render thread (prio 1) would outrank it, since `#[esp_rtos::main]`
    // starts at priority 0.
    #[cfg(feature = "ui-thread")]
    sched::raise_app_executor();

    #[cfg(not(feature = "ui-flush-thread"))]
    {
        let int_exec = make_static!(esp_rtos::embassy::InterruptExecutor::new(
            board.system.sw_int.software_interrupt1
        ));
        int_exec
            .start(esp_hal::interrupt::Priority::min())
            .spawn(flush_task().expect("spawn flush task"));
    }
    #[cfg(feature = "ui-flush-thread")]
    // SAFETY: `flush_thread` runs an executor and never returns.
    unsafe {
        sched::spawn("ui-flush", flush_thread, sched::PRIO_FLUSH, sched::FLUSH_STACK, 0)
    };
    // The APP core needs its own scheduler before a thread can be pinned there.
    // SWI1 is free in this mode: the flush runs on a thread, not an interrupt
    // executor, so nothing else claims it.
    #[cfg(feature = "ui-app-core")]
    {
        // Park the APP core first. A JTAG-flashed boot can leave its control
        // registers reporting "running", and esp-rtos then panics
        // `CoreAlreadyRunning` — the same hazard `board::multicore` documents.
        // SAFETY: we are on PRO, parking the other core, before handing the real
        // peripheral to esp-rtos; `start_second_core` unparks it.
        unsafe {
            esp_hal::system::CpuControl::new(esp_hal::peripherals::CPU_CTRL::steal())
                .park_core(esp_hal::system::Cpu::AppCpu);
        }
        let stack = make_static!(esp_hal::system::Stack::<8192>::new());
        esp_rtos::start_second_core(
            board.system.cpu_ctrl,
            board.system.sw_int.software_interrupt1,
            stack,
            || {},
        );
    }
    #[cfg(feature = "ui-thread")]
    // SAFETY: `render_thread` runs an executor and never returns.
    unsafe {
        sched::spawn(
            "ui-render",
            render_thread,
            sched::PRIO_RENDER,
            sched::RENDER_STACK,
            sched::RENDER_CORE,
        )
    };

    spawner.spawn(latency_probe().expect("spawn latency probe"));
    spawner.spawn(stats_task().expect("spawn stats task"));

    // Baseline keeps the render loop on this executor — that is the whole point
    // of the comparison. The threaded modes leave it idle for the probe.
    #[cfg(not(feature = "ui-thread"))]
    render_loop().await;
    #[cfg(feature = "ui-thread")]
    core::future::pending::<()>().await
}
