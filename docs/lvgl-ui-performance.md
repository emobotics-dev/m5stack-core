<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# LVGL UI performance on m5stack-core

Everything here is measured on the bench, both boards, and reproducible with
**`examples/demos/src/bin/lvgl_sched.rs`** — which is a *pipeline stress
harness*, not a UI demo. It exists to load the render/flush path in known ways
and report what that costs: a sweeping gauge for redraw load, a 10 ms-period
probe task standing in for latency-sensitive application work, and load profiles
cycled at runtime so every number comes from one flash under identical
conditions.

The profiles vary one thing each — nothing, a plain fill, text, a small arc, the
same arc enlarged, a full-screen invalidate, and eight independent objects — so
the cost model below is read off the hardware rather than argued.

Context: issue #63, PR #64.

## The rules, in one screen

1. **Cost scales with the number of *animated objects per frame*, not with
   pixels.** A dense screen with three moving elements costs the same as a
   sparse one with three moving elements. Making a widget *bigger* is nearly
   free; adding another animated one is not. Measured directly — one animated
   bar against eight, on the same screen:

   | | draw | pixels/s | draw tasks |
   |---|---:|---:|---:|
   | 1 bar | 7-8 % | 104 160 | 31/s |
   | 8 bars | **54-57 %** | **69 720** | **249/s** |

   Eight times the objects, eight times the draw tasks, ~7x the cost — while
   drawing *fewer* pixels.
2. **Budget ~6 animated widgets at 30 Hz per half a core**, ~14 to saturate
   one (8 measured at ~55 %).
3. **Stagger update rates.** Cost is linear in draw tasks per *second*, so a
   widget refreshed at 5 Hz costs a sixth of one at 30 Hz. Most dashboard
   values do not change at 30 Hz. This is where the headroom is.
4. **Widget choice is a ~4x lever.** An arc draw task is 425 us, a fill 108 us —
   independent of area. Arcs, rounded corners, shadows and gradients are the
   mask-based, expensive family.
5. **Run the render loop on its own thread, below the latency-sensitive work.**
   Not on the shared executor, and not on an interrupt executor.

## Where the time actually goes

Per draw task, CoreS3, measured with LVGL's own profiler:

| | exclusive | per call |
|---|---:|---:|
| `lv_draw_sw_arc` (mask construction) | 26.4 ms/s | **425 us** |
| `lv_draw_sw_blend` (the pixels) | 13.2 ms/s | 6 us |
| `sw:fill` | 6.7 ms/s | 108 us |
| LVGL object/event machinery | ~30 % of draw | |

Blending is **12 %** of render time. The expensive part is geometry —
anti-aliased mask construction — which is why enlarging a shape barely moves
the cost while adding a shape moves it a lot.

Consequently a small dirty area looks catastrophic per pixel (~830 cyc/px) and
a full-screen redraw looks cheap (~61 cyc/px). Both are the same fixed
per-draw-task cost divided by different pixel counts. Do not tune against
cycles-per-pixel.

## Scheduling

`#[esp_rtos::main]` starts at priority **zero — the lowest**. A render thread at
any priority therefore outranks the app executor unless the app executor is
raised. The ladder that works:

| | priority | |
|---|---|---|
| esp-radio blob threads | ~20+ | untouched, always win |
| app / latency-sensitive work | **3** | raised via `CurrentThreadHandle::set_priority` |
| flush thread | **2** | above render, so the panel never starves |
| render thread | **1** | yields to everything above |
| idle | 0 | |

Keeping the whole ladder inside 1..=3 is what makes UI-versus-radio safe by
construction rather than by tuning.

Two details are load-bearing:

- **Threads, not another `InterruptExecutor`.** An interrupt executor makes the
  UI preempt *everything*, which is backwards.
- **Block on an RTOS semaphore, not `waiti`.** `oxivgl`'s stock flush-wait parks
  the core until an interrupt: the scheduler is never entered, so for the whole
  15–30 ms transfer **nothing else runs at all**. That single change is the
  largest part of the result below.

Measured, 12 one-second samples per mode, against a 10 ms-period probe task:

| | PRO | probe mean | probe max | wakeups/s |
|---|---|---|---|---|
| shared executor (naive) | 70 % | 4700 us | 14000 us | 86/100 |
| render thread + ladder | 72 % | 145 us | 630 us | 100/100 |
| + flush thread | 74 % | 119 us | 196 us | 100/100 |
| + render on APP core | **12 %** | 105 us | 290 us | 100/100 |

Fire27 starts worse (baseline drops 40 % of wakeups, exceeds 20 ms every
second) and gains more: 138 us mean, 211 us max, none missed.

Note the baseline was **never CPU-starved** — 70 % busy, ~30 % idle, and still
missing 14 % of a 10 ms deadline. This is a blocking problem, not a throughput
one.

## Knobs that work

**Frame rate is the CPU knob.** Set the refresh period at runtime through the
display's own timer (`lv_display_get_refr_timer` + `lv_timer_set_period`) rather
than `lv_conf.h`, so it stays per-application. Holding 31 fps instead of 59
halves the render work: 74 % → 42 % on CoreS3, 96 % → 58 % on Fire27.

**Render on the APP core** if the application can spare it: PRO 42 % → 12 %, for
~20 us of extra cross-core latency. LVGL stays single-threaded — only which core
that one thread runs on changes.

**IRAM for LVGL's hot code** (`LV_ATTRIBUTE_FAST_MEM` → `.rwtext`). Know the
price before taking it:

| | draw | IRAM cost | share of IRAM |
|---|---|---|---|
| CoreS3 | −13 % | 33.6 kB | 10 % of 328 kB |
| Fire27 | −5 % | 33.0 kB | **26 % of 127 kB** |

Worth it on ESP32-S3. On ESP32 it is a quarter of all IRAM for 5 % — take it
only if the application has IRAM to spare.

## Knobs that do not work

Each was implemented and measured. They are recorded so they are not retried.

| | result |
|---|---|
| 80 MHz display SPI | **Blanks the panel on both boards.** Counters report full throughput throughout — only a camera catches it. Not the SD card (removed), not the GPIO matrix (SPI3/IOMUX raised the clock and it still failed), not drive strength (40 mA changed nothing). The panel is the limit; 40 MHz stands. There is no 50 MHz — the divider yields nothing between 40 and 80. |
| Parallel draw units (`LV_DRAW_SW_DRAW_UNIT_CNT > 1`) | No frame-rate gain at all, +30 points of PRO. The heavy case is DMA-bound (3.4 MB/s of a ~5 MB/s SPI); the light case has ~4 draw tasks a frame, too few to split. Needs `LV_USE_OS`, which itself costs ~5 points. |
| ESP32-S3 SIMD | **Cannot be used.** `xtensa-lx-rt` does not save the PIE `q0`–`q7` register file on context switch (see issue #66). Espressif's patch also targets the RGB565 hooks, which a RGB565_SWAPPED configuration never reaches. |
| Hand-written blend | Parity at best over four iterations (828–857 vs LVGL's 828–838 cyc/px). LVGL's loop is already the one-multiply packed trick with a pairwise mask skip. |
| LVGL caches (circle/style/image), stride alignment | No measurable effect on this workload. |
| `-O3` for LVGL's C | ~1–2 % for **+111 kB** of flash. The size-tuned profile is the better default. |
| Larger render buffer | Blocked by DMA descriptor sizing, not memory — an 80-line buffer panics `InsufficientDescriptors`. |

## Profiling your own UI

`lvgl_sched` carries two instrumentation layers, both **off by default** because
they cost ~2 points of draw:

- **`ui/lvprof.rs`** — backs LVGL's own profiler hooks and records *exclusive*
  time per tag. Enable with `LV_USE_PROFILER 1` in `conf/lv_conf.h`. It warns
  and refuses its totals if the tag table overflowed, so a partial account is
  never mistaken for a complete one.
- **`ui/lvasm.rs`** — per-hook blend call counters. Enable with
  `LV_USE_DRAW_SW_ASM LV_DRAW_SW_ASM_CUSTOM`. This is how the masked-fill path
  was identified as the hot one (2103 calls/s against 62 for everything else).

Measure before optimising: on this workload every intuition about where the
time went was wrong, including which of blending, byte order, buffer size and
polling rate mattered. All four were measured and none was the answer.
