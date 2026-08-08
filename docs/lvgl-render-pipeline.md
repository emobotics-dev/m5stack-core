<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# The LVGL render pipeline

**Run LVGL on two threads: render below the application, flush above it.** This
is the arrangement every new UI should use. Reference implementation:
[`examples/lvgl_threads.rs`](../examples/lvgl_threads.rs) — the pattern with no
measurement apparatus. What it costs and why: [LVGL UI
performance](lvgl-ui-performance.md).

The pipeline itself is **oxivgl's** (`FlushSync`, `Ui::init`/`Ui::run`,
`RenderConfig`). Adopting it is a change to `main`, not to any `View`. What stays
application-side is *placement* — thread creation, the priority ladder, core
pinning — because only the application knows what the UI must yield to.

## The shape

| | runs on | priority |
|---|---|---|
| esp-radio blob threads | own threads | ~20+ (untouched) |
| application / latency-sensitive work | `#[esp_rtos::main]` executor | **3** |
| flush | own thread | **2** |
| render (all LVGL calls) | own thread | **1** |
| idle | | 0 |

The whole UI ladder stays inside `1..=3`, so it cannot starve the radio by
construction rather than by tuning. Constants live in
[`examples/common/sched.rs`](../examples/common/sched.rs) (`PRIO_APP`,
`PRIO_FLUSH`, `PRIO_RENDER`, the stack sizes, `RENDER_CORE`).

## Three steps in `main`

Three layers meet in `main`, and every line below is tagged with the one it
belongs to: **`[oxivgl]`** the pipeline, **`[BSP]`** the panel, **`[YOURS]`** the
placement you own (plus **`[runtime]`** for embassy/esp-rtos boilerplate). Only
the `[YOURS]` lines are new work when porting an application.

```rust
// [oxivgl]  the pipeline — rendering, the flush loop, the flush-wait strategy.
use oxivgl::display::LvglBuffers;
use oxivgl::flush_pipeline::{SemaphoreFlushSync, flush_frame_buffer, set_flush_sync};
use oxivgl::view::{RenderConfig, Ui, View};

// [BSP]     the panel: bring-up and screen geometry. m5stack-core.
use m5stack_core::board::display::{SCREEN_H, SCREEN_W};

// [YOURS]   placement — which thread, which priority, which core. Not a crate:
//           in the examples it is `examples/common/sched.rs`, ~60 lines
//           (priority constants, stack sizes, a `task_create` wrapper) that an
//           application copies and then owns, because only it knows what the UI
//           must yield to.
use crate::common::sched;

// [BSP]     board bring-up as usual, then hand the panel to the flush thread.
DISPLAY.try_send(DisplayDriver::new(dbus)).ok();

// 1. [oxivgl] Before the LVGL display exists — i.e. before the render thread
//    reaches `Ui::init`; the registration is read by the wait callback and the
//    flush. `leak_thread` because the flush is a thread; an ISR needs `leak_isr`.
set_flush_sync(SemaphoreFlushSync::leak_thread());

// 2. [YOURS] Before the UI threads exist. `#[esp_rtos::main]` starts at
//    priority 0 — the *lowest* — so without this the UI outranks the work it
//    must yield to. One call: `CurrentThreadHandle::get().set_priority(3)`.
sched::raise_app_executor();

// 3. [YOURS] Threads, not the shared executor and not an InterruptExecutor.
//    `sched::spawn` is a thin wrapper over `esp_radio_rtos_driver::task_create`.
// SAFETY: both entries run an executor and never return.
unsafe {
    sched::spawn("ui-flush", flush_thread, sched::PRIO_FLUSH, sched::FLUSH_STACK, 0);
    sched::spawn("ui-render", render_thread, sched::PRIO_RENDER, sched::RENDER_STACK,
                 sched::RENDER_CORE);
}
```

A thread entry is an `extern "C" fn` and cannot capture, so each one starts its
own executor and the display reaches the flush side through a `Channel`:

```rust
// [runtime] thread entry, executor, and the hand-over channel.
use core::ffi::c_void;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use static_cell::make_static;
// [YOURS]   the DisplayOutput impl and the LVGL buffer size derived from the
//           BSP's screen geometry — `examples/common/ui/`.
use crate::common::ui::{DisplayDriver, LVGL_BUF_BYTES};

static DISPLAY: Channel<CriticalSectionRawMutex, DisplayDriver, 1> = Channel::new();

#[embassy_executor::task]
async fn render_task() -> ! {
    static mut BUFS: LvglBuffers<{ LVGL_BUF_BYTES }> = LvglBuffers::new();
    // SAFETY: touched only here, before this thread takes sole ownership of
    // LVGL for the rest of the program.
    let bufs = unsafe { &mut *core::ptr::addr_of_mut!(BUFS) };

    // [oxivgl] `Ui::init` must run *here*, not in `main`: every later LVGL call
    // happens on this thread. A `run_app(…)` call becomes exactly this pair.
    Ui::init(SCREEN_W.into(), SCREEN_H.into(), bufs)
        .run(MyView::default(), RenderConfig::default().with_target_fps(30))
        .await
}

#[embassy_executor::task]
async fn flush_task() -> ! {
    // [oxivgl] the whole flush side: drain the draw channel, push to the panel.
    flush_frame_buffer(DISPLAY.receive().await).await
}

// [runtime] boilerplate: an entry cannot capture, so it starts its own executor.
extern "C" fn render_thread(_: *mut c_void) {
    let exec = make_static!(esp_rtos::embassy::Executor::new());
    exec.run(|s| s.spawn(render_task().expect("spawn render")))
}
// `flush_thread` is the same four lines around `flush_task`.
```

`DisplayDriver` is the application's `DisplayOutput` impl over the BSP display
(`examples/common/ui/driver.rs`); `MyView` is ordinary oxivgl `View` code and
knows nothing about any of this.

## Why each step is load-bearing

- **`SemaphoreFlushSync`, not the default.** oxivgl defaults to parking the core
  in `waiti 0` for applications that link no scheduler. That parks it for the
  whole 15–30 ms panel transfer, during which *nothing but ISRs runs*. Blocking
  on an RTOS semaphore instead is the single largest part of the win.
- **Raise the app executor.** Skip it and the change makes latency **worse** —
  the UI threads land above the application.
- **Threads, not an `InterruptExecutor`.** An interrupt executor makes the UI
  preempt everything, which is backwards; a thread is preemptible by priority in
  both directions.
- **Flush above render**, so the panel never starves waiting on rasterisation.

Measured against a 10 ms-period probe task, worst-case wakeup latency:
14 000 µs (render+flush on the shared executor) → 196 µs. Frame rate is
unchanged; this is a blocking problem, not a throughput one.

## Board notes

- **Set the frame rate explicitly** — `RenderConfig::with_target_fps`, not
  `lv_conf.h`, so it stays per-application. 31 fps instead of 59 halves the
  render cost.
- **CoreS3: pin render to the APP core** (`ex-ui-app-core` → `RENDER_CORE = 1`)
  if the application can spare it — PRO 42 % → 12 % for ~20 µs of extra
  cross-core latency. LVGL stays single-threaded; only which core that one
  thread runs on changes.
- **Fire27 runs both threads on PRO** — `ui-app-core` hangs there (#65).

## The previous arrangement

Render on the shared `#[esp_rtos::main]` executor with the flush on a
high-priority `InterruptExecutor`. Still what `examples/lvgl/` does, and it does
hold ~30 fps — but everything else on that executor waits milliseconds for a
panel transfer, which is what #63 was filed against. Documented because the
example still uses it; do not start there.
