# Changelog

All notable changes to this crate are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.0] - 2026-08-22

### Known issue

- **CoreS3 requires `esp32s3 = "=0.35.2"` in the consumer's manifest.** The PAC's
  0.35.3 renamed `USB0` to `USB_FS` and de-prefixed the RTC_CNTL `EXT_WAKEUP1`
  accessors — renames only, same address and register layout, but shipped in a
  *patch* release. esp-hal 1.1.2 still calls the old names and asks only for
  `^0.35`, so a fresh resolve fails to compile (`E0433`, `E0599`). Not specific
  to this crate: reproducible with nothing but stock `esp-hal =1.1.2` and
  `esp32s3 =0.35.3`, and anything on esp-hal 1.1.x for ESP32-S3 is affected.
  Fire27 is unaffected.

  The bound is deliberately **not** carried here — this crate calls no PAC API,
  so the constraint belongs to esp-hal, and pinning it here would block esp32s3
  0.36 downstream long after the cause is gone. Upstream has already merged the
  adaptation (esp-pacs #444, esp-hal #5558); drop the pin when an esp-hal release
  carries it. See the README for the full explanation.

### Added

- **`mem::init_heap_sized!`** (#79): registers the global heap's DRAM regions at
  explicit sizes, for a binary whose own workload sets the number and which no
  `HeapProfile` can express. A macro rather than a function because
  `heap_allocator!` sizes a `static`, so the size has to arrive as a literal at
  the expansion site. `esp_alloc` and `esp_hal` are re-exported from the crate
  root so the macro resolves them at the caller without the caller importing
  either. `init_heap` keeps its signature and its sizes; the profiles now route
  through the new macro, so it is the path they all take rather than a second
  implementation beside them — if it breaks, every profile breaks with it.
- **PPS identity probe** (#78): `driver::pps::MODULE_ID` and
  `PpsDriver::probe()`, which reads registers 0x00/0x01 and requires `0x1041` —
  the value the module's own firmware hardcodes. It is the only register whose
  value does not depend on a conversion, so a coherent answer proves address,
  framing and byte order before any reading is believed. `io::pps::pps_loop`
  runs it at startup, retried 3× at the loop cadence; exhaustion logs and
  carries on, because the loop reaches the same verdict through the normal
  error path. The probe validates identity, not per-sample validity — a module
  can pass it and still return a garbage batch seconds later.
- `PpsError::is_transient()` — true for `NotReady` / `OutOfRange`, which are
  kept off `pps_loop`'s consecutive-error budget. That budget exists to shut
  down a bus with nothing on it; a module that is present and merely still
  converting is healthy, and spending the budget on it would turn a rare bad
  sample into a reliably dead PPS task.
- `PpsReadings::rejected` — readings discarded since boot, riding along on the
  next good batch. A rejection was otherwise visible only on the console, which
  is not attached in a vehicle.
- **Fair SPI2 arbitration** (#86): the display and the SD card queue for the
  shared bus in arrival order, so neither can monopolise it. `board::spi2`
  gained `permit_stats() -> PermitStats` — arrivals, grants and releases per
  side, where `got - rel` is what that side holds right now. That separates a
  holder parked mid-command from a permit lost outright from a queue whose head
  is never polled: three failures that look identical from outside and were
  being told apart by argument. Diagnostic only — nothing reads it to decide
  anything, so a torn read costs a wrong log line, never behaviour.
- **`PreparedCard::acquire() -> Option<Spi2CardGuard>`** (#86) hands out the bus
  and the chip-select together. An `SpiDevice` drops CS on exit, so a driver
  that must hold it across command, token and payload could not express that
  through one.

### Changed

- **Breaking:** `PpsReadings` gained a public field and `PpsError` gained three
  variants (`NotReady`, `OutOfRange`, `WrongDevice`). Neither type is
  `#[non_exhaustive]`, so a consumer that writes a `PpsReadings` literal or
  matches `PpsError` exhaustively must be updated — this release takes a minor
  bump, not a patch.
- **MSRV raised to 1.96**, and `mem::PsramSafe` now denies atomics through the
  generic `Atomic<T>` rather than the ten aliases. From 1.96 every atomic is an
  alias of that one type, so `impl !PsramSafe for AtomicU32` reads as an impl on
  `Atomic<u32>` — a specialization, which negative impls forbid (`E0366`), and
  the crate no longer built on 1.96 or later at all. The rule it replaces was
  also incomplete: `AtomicI64`/`AtomicU64` were never listed, so on a target
  that has them the check failed open. Behaviour is otherwise unchanged —
  `UnsafeCell`, `Cell` and `RefCell` stay `PsramSafe`, and a reference or
  pointer to an atomic still is too.
- **Breaking:** `io::rpm` reports the pulse rate the hardware measures, not
  engine rpm (#86). `read_rpm` → `read_pulse_hz`, `rpm_loop` → `pulse_loop`,
  `on_rpm` → `on_pulse_hz`, and `RpmConfig` loses `pole_pairs` and
  `pulley_ratio`. Pole pairs and belt ratio are facts about the alternator and
  the engine it is bolted to, not about the board, so a BSP that knows them
  cannot be reused on another vehicle; the application owns the conversion. The
  value changes meaning as well as name — but every name changed too, so a stale
  caller fails to compile rather than quietly reporting Hz as rpm.
- **Breaking:** `SharedI2cBus` is no longer `Send`/`Sync` (#86, closes #83).
  Both claims the old `unsafe impl`s rested on had lapsed: I2C bring-up moved to
  the APP core, and cooperative scheduling is a statement about *access* while
  `Send` is one about moving a value between threads. `!Send` is now the
  compiler's to enforce, so moving the bus to another core is a build error
  rather than unchecked breakage.
- **Breaking:** `PreparedCard` is reshaped and `into_inner` now yields
  `FairSpiDevice<CardSpiDevice<PresenceCs<CS>>>` (#86) — the fairness wrapper is
  part of the type the consumer receives.
- **Breaking:** `board::spi2::Spi2Resources::into_display_only` returns
  `Result<_, DisplayOnlyError>` instead of `.expect()`-ing the SPI configure step
  (#86). A library crate must not panic on a caller's bad config
  (`conventions/rust-code.md` §6); `lcd_async`'s own error type has no variant
  for it, hence the wrapper.
- **esp-hal family pinned to `=1.1.2`** (#88), up from `=1.1.1`. This is
  consumer-visible because the crate re-exports `esp_hal`. The fork the examples
  patch to had already rebased onto 1.1.2, so a consumer patching the family to
  it by path got a pin no patch satisfied: cargo resolved a *second* esp-hal
  1.1.1 from crates.io and the build died on the `links = "esp-riscv-rt"`
  collision rather than on anything legible. `=1.1.2` is a real crates.io
  release, so the publish gate is unaffected.

### Fixed

- **A PPS reading the module never measured is no longer published as fact**
  (#78). `evaluate_result` turned any four bytes into an `f32`, so a batch in
  which the module answered perfectly at the I2C level while having nothing to
  report was handed up as data. Seen in the field once in ~10 000 samples: four
  NaNs plus a plausible-looking 31.36 V input voltage against a 26.51 V
  battery — enough to latch a consumer's over-voltage fault with the engine
  running. No I2C-level check could have caught it, because the transaction
  succeeded: the module ACKed and returned four well-formed bytes, and rubbish
  is bit-identical to a real reading there. The missing check was at the decode
  boundary, so readings are now rejected unless finite and within the range the
  hardware can produce. Since `read_pps` is all-or-nothing, rejecting one value
  discards the whole batch, which is what keeps the bad input voltage from
  escaping. The range check is deliberately loose and would *not* have caught
  31.36 V — it is a second net for wilder corruption, not the fix.
- **Per-module log filtering works again** (#86). It died silently when
  `esp-println` was dropped: that logger parsed `ESP_LOG`, ours hardcoded
  `Info`, and every `debug!` on target was discarded for eleven weeks with
  nothing anywhere failing. The console is the log backend, so the mechanism
  lives there and the policy string comes from the application:
  `io::console::init(spec)` takes an `RUST_LOG`-style filter. `init` now emits
  one `debug!`-level self-test marker, so a harness asserting it turns a dead
  filter into a loud failure rather than silence. The check stays cheap — a
  record at or above the default level returns immediately, and with no
  per-target directive that is the whole of it.
- **SD initialisation no longer runs at the display's clock** (#86). The display
  leaves 40 MHz on the bus, so SD init timed out five times out of five. The
  card's terms are restated on every acquire, and which user the peripheral is
  configured for lives in the mutex payload beside the bus, so the record and
  the hardware change under one lock and cannot disagree.
- **A display flush no longer starves the SD busy-poll** (#86). The mutex is not
  FIFO and the two users are asymmetric — a busy poll asks ~1000×/s while a
  flush holds the bus for a whole panel transfer. Measured before the fix:
  fire27 `:format` drifted from 440 ms to 9434 ms while cores3 sat unchanged.
- **A >20 s "STALL, card state unknown" is fixed at its cause** (#86).
  `apply_config` reprogrammed the clock divider with no idle check, and on esp32
  the clock-gate sequence around that write is `cfg`'d out; reprogramming a
  peripheral that has not finished left the next transfer armed but never
  completing, parked in the `TransferDone` wait while holding the bus. Letting
  the bus go idle first is the fix — making the reconfigure conditional merely
  cut the rate and left the fault latent.
- **CoreS3 builds against the current esp32s3 PAC again.** 0.35.3 renamed the
  `USB0` peripheral to `USB_FS` and dropped the redundant prefix from the
  RTC_CNTL `EXT_WAKEUP1` accessors (`ext_wakeup1_sel` → `sel`,
  `ext_wakeup1_status_clr` → `status_clr`), so esp-hal — which asks only for
  `^0.35` — stopped compiling (`E0433`, `E0599`) the moment a fresh resolve
  picked it up. Reproduced with nothing but stock `esp-hal =1.1.2` and
  `esp32s3 =0.35.3`, so it is a property of neither this crate nor the fork.

  These are renames and nothing more: the same address (`0x6008_0000`), a
  byte-identical register block, and the same svd2rust build. So the fork
  **follows** them rather than holding the PAC back — the metadata's `pac`
  override carries the peripheral rename, leaving esp-hal's own
  `peripherals::USB0` singleton and `otg_fs` untouched. `[patch.crates-io]`
  moves onto that commit, and a fresh resolve now takes 0.35.3.

  This crate declares no dependency on `esp32s3` and calls no PAC API; the
  constraint belongs to esp-hal, which names it. A consumer of the **published**
  crate still resolves stock esp-hal 1.1.2 and remains affected until the fix is
  upstream.
- **CoreS3 card responses no longer read back `0x00`** (#86). GPIO35 is both
  MISO and the display DC line, and the DC write used to take effect while the
  card was mid-command; it is now applied under the bus permit.

## [0.5.0] - 2026-08-01

Version 0.4.3 was prepared but never released — it was bumped and given a
changelog section, then never tagged, mirrored or published, so no user could
ever obtain it. Its one change is folded in below rather than left pointing at
a version that does not exist. The last release on crates.io was 0.4.2.

### Added

- **Firmware identity reporting** (#43): `app_elf_sha256()` — the built
  image's own content hash, straight off the esp-idf application descriptor,
  needs nothing from the consumer. New `app-desc` feature (implied by `heap`,
  as before) decouples the descriptor from the global heap, so a board that
  wants identity reporting without `esp-alloc` in the graph can enable just
  this.
- **`identity` feature** (implies `app-desc`): makes `app_desc!()`'s version
  field `<pkg>/<bin>/<features>/<hash><dirty>` (e.g.
  `demos/display/crypto-opt/0f63a4926303+`) instead of plain
  `CARGO_PKG_VERSION` — same call site, no application-code change.
  Forgetting the required `build.rs` wiring is a compile error (`env!()` has
  nothing to read), not a silent gap; a combination that doesn't fit the
  descriptor's fixed 31-byte field is also a compile error (a `const`
  assertion at the `app_desc!()` call site), not a silent truncation. Off by
  default; existing consumers are unaffected.
- **`project_name` is now `CARGO_BIN_NAME`**, not the package name, in both
  the default and `identity` cases — a package can have more than one
  `[[bin]]`, and only the per-binary compilation (where `app_desc!()`
  expands) knows which one is being built. The crate's own `CARGO_PKG_VERSION`
  is still logged (`version=`) even under `identity`, where the descriptor's
  own `version` field no longer holds it — `app_desc!()` exports it under a
  second small linker symbol for the boot log to read back.
- **New crate `m5stack-core-build`**: a host-only build-time helper —
  `emit_identity_env(features: &str, hash_len: usize)`, called once from a
  consumer's own `build.rs`, sets the `<features>/<hash><dirty>` env var
  `identity` requires (the package/binary names are joined in separately, by
  `app_desc!()` itself, since a `build.rs` runs once per package and can't
  know which binary it's describing). `hash_len` is another lever for the
  31-byte budget — the commit-hash abbreviation width, honoured **exactly**
  (clamped to a floor of 4). Optional; the env var can also be set by hand.

  Git state comes from [`vergen-gitcl`] rather than a hand-rolled `git`
  invocation (#48). That fixes a needless rebuild: the `rerun-if-changed` paths were
  derived from `CARGO_MANIFEST_DIR`, so for any consumer that is not the
  repository root — a workspace member, as `examples/demos` is — they named a
  `.git` that does not exist, and cargo re-ran the build script on *every*
  build. Measured here on a second, untouched build of one demo: 4.97 s before,
  0.18 s after. It also makes the width exact: `git rev-parse --short=<n>`
  treats `n` as a minimum and lengthens an ambiguous prefix, which could have
  spent the 31-byte budget as a repository aged.

[`vergen-gitcl`]: https://crates.io/crates/vergen-gitcl
- **Automatic boot-time identity log** (#43): `io::console::install` now logs
  the descriptor once — package/binary + git mark (or plain binary +
  version), and an `app_elf_sha256` prefix — as the first BSP-emitted line,
  right after the transport comes up and before the application's own
  logging, whenever `app-desc` is on (`markers::IDENTITY`). Requires
  `app_desc!()` to have been invoked somewhere in the binary; if it wasn't,
  this is a **link** error, not a missing log line — any consumer already
  enabling `app-desc`/`heap` is expected to call it.
- **`app_desc!("prefix")`** — an optional string-literal argument overrides
  the automatic `CARGO_PKG_NAME`/`CARGO_BIN_NAME` join in the `identity`
  mark, e.g. `app_desc!("oxichg/evcc-hl")` instead of the real (and, for a
  real project, often too long to fit the 31-byte field)
  `oxicharge-cores3/evcc-headless`. Nothing here abbreviates automatically —
  this is the caller's explicit choice, same reasoning as everywhere else in
  this design. `project_name` is unaffected, still the real `CARGO_BIN_NAME`.
  Default (no argument) is unchanged.
- **`app_desc()` is now public** — reads back the whole descriptor
  (`EspAppDesc`), not just the `app_elf_sha256()` convenience wrapper.
- **New crate `m5stack-core-hil` — a hardware-in-the-loop harness for both
  boards** (#46, #47, #55), binary `m5stack-hil`. A *host* crate: it is not
  part of the published library and adds nothing to a consumer's dependency
  graph, but it is what makes a claim like "verified on hardware" mean
  something specific in this repo. It owns the three things an ad-hoc
  `espflash` invocation gets wrong:

  - **Claiming a board.** One owner per board, keyed to the MAC rather than a
    `ttyACM` index (those renumber on replug). A live holder is *refused and
    reported* — never reclaimed, because killing the holder destroys another
    session's capture — while a dead holder's lock is taken over and the
    takeover is said out loud. Released on every exit path, including panics.
  - **Flashing only when needed.** `--ensure-image` compares the board's
    running `app_elf_sha256` against the hash of the ELF about to be flashed
    and skips the write when they already match, turning a ~15 s cycle into
    ~6 s. This is the first real consumer of the `identity` work above.
  - **Losing no output.** The harness attaches to the console *before* it
    resets, because the ESP32-S3's USB-Serial-JTAG discards output when no
    host is reading — reset-then-open captured **0 bytes** from a board
    emitting 1788 per boot. Before each reset the port is read until silent,
    then a cursor barrier is drawn: without that, a line left over from the
    *previous* boot satisfies a match and turns a broken build into a pass. A
    board that never goes quiet is reported as such rather than silently
    treated as clean.

  Both boards are covered, differing only in how the reset is delivered:
  CoreS3 over JTAG with the probe named explicitly (an unqualified
  `probe-rs reset` picks one of the probes on a shared bench, i.e. possibly
  someone else's board), Fire27 by pulsing RTS on the port the harness already
  holds — its USB-serial bridge is a separate chip from the ESP32, so the tty
  survives the reset. Verified on the bench with a negative control: stub the
  pulse and the identity is never captured, which rules out the reset being an
  artefact of the DTR edge that opening a tty produces.

  Boards are **named** in a gitignored `hil.toml` (see `hil.toml.example`), so
  swapping hardware or adding a rig is a config edit, not a code edit. Wrappers:
  `tools/cores3-run.sh`, `tools/fire27-run.sh`, `tools/hil.sh`.

### Changed

- **BREAKING: `mem::init_heap` no longer takes PSRAM, and PSRAM is never
  registered with the global allocator by default** (#41). Getting PSRAM into
  the global heap is a hazard for the *whole* crate graph, not just the
  caller — once a region carries external capability, every plain
  `alloc::vec!` / `Box` / `String` anywhere becomes eligible to silently spill
  into it once internal DRAM runs out (esp-alloc has no "external, but not for
  capability-less requests" region flag). `init_psram_heap` (map + register
  everything globally in one call) is removed.
  - `init_heap(profile, psram)` → `init_heap(profile)`. Binaries that don't
    need PSRAM: just drop the second argument.
  - New default: `mem::psram_map(psram) -> &'static mut [MaybeUninit<u8>]` —
    maps the whole region as a private slice, never touches the global heap.
  - `psram_split(psram, reserve: Option<usize>)` → `psram_split(psram,
    reserve: usize)`. The old `reserve: None` (fully private) is now
    `psram_map`; the old `reserve: Some(n)` is `psram_split(psram, n)`.
- **esp-hal fork rev bumped to `2fca1aa4`.** One of the six commits it picks
  up changes behaviour on both boards: the I2C driver now recovers the bus
  after **every** failed transaction, a NACK included. Earlier revisions
  skipped that recovery on ESP32 and rate-limited it on ESP32-S3, on the
  belief that its chip-wide `PeripheralClockControl::reset` fallback desynced
  the WiFi/BLE blob (#3, #10). That was a misattribution — the register it
  touches (`SYSTEM.PERIP_RST_EN`) carries no radio bits, radio reset and clock
  living in separate DPORT registers, and 30k+ resets under WiFi+BLE coex on
  both boards produced no wedge. The real fault was contention between the I2C
  completion IRQ and the radio controller's IRQ on one core, which is
  application-side: it is why `Board::i2c0` is handed out blocking on both
  boards, with the core-binding rule stated on the field. The practical
  effect of the bump is that an un-recovered NACK can no longer poison later
  transactions on the shared bus. Also included: two USB-Serial-JTAG
  async-write fixes (a lost wakeup, and an `int_ena` race that could deafen
  the reader) and ESP32 SPI-slave DMA arm-sequence parity with ESP-IDF (#21).
- **`documentation` now points at <https://emobotics-dev.github.io/m5stack-core>**
  (#36) — self-hosted rustdoc, one tree per board. docs.rs has failed for every
  published version and cannot be fixed: its stock toolchain has no Xtensa
  backend, and both boards are Xtensa-only, so there is no RISC-V
  `default-target` to fall back on the way esp-hal has. Prepared for 0.4.3;
  since that version was never published, crates.io still showed no
  documentation link at all, and this is the release that actually delivers it.

## [0.4.2] - 2026-07-29

Additive — one unified, cross-driver button-timing API so latency-sensitive
input (encoders) can opt out of the multi-tap counting delay on either board.
No breaking changes.

### Added

- **`Board::spi3` / `Board::spi3_dma`** (#27) — the spare SPI3 unit + its DMA
  channel, previously moved into `Board::split` and dropped with no way for
  an app to claim them. Listed the same way as `Board::rmt`/`Board::pcnt`: the
  BSP does not drive it, wiring/IRQ-priority/core-allocation stays with the
  app. `spi3_dma` is `AnyGdmaChannel` on CoreS3 (`DMA_CH1`, GDMA channels are
  interchangeable) and `AnySpiDmaChannel` on Fire27 (`DMA_SPI3`, its own
  dedicated PDMA unit). No CS field — take a pin from `Board::m5bus` and
  attach it as the peripheral's hardware chip-select.
- **`io::buttons::ButtonTiming { long_press_ms, multi_tap_ms }`** (#58) — a
  cross-driver press-decoding timing shared by both front-panel drivers, with
  presets `ButtonTiming::multi_tap()` (count consecutive taps) and
  `ButtonTiming::immediate_single_press()` (`multi_tap_ms: 0` → each press emits
  `ButtonAction::Short(1)` the instant it debounces, no counting delay). Both
  drivers count taps by default, adding the multi-tap window (~350 ms Fire27 /
  300 ms CoreS3) to *every* single press — unusable for an encoder, where a
  downstream accumulator sums immediate single presses into multi-step.
  - **`ButtonResources::into_buttons_with_timing(timing)` (Fire27, feature
    `buttons`)** — applies a `ButtonTiming`; the button timings were previously
    hardcoded (`into_buttons()` remains, keeping async-button's defaults). `mode`
    is forced to `PullUp` — the BSP owns the active-low wiring, the caller owns
    only timings.
  - **`TouchButtons::with_timing(i2c, timing)` /
    `TouchButtonsConfig::with_timing(timing)` (CoreS3)** — the touch-side
    counterpart; `TouchButtons::new(i2c, TouchButtonsConfig::default())` and all
    existing config fields are unchanged.

### Changed

- **Dependencies tracked to latest** (only esp-hal 1.1.1 and its stack are
  pinned): embassy-time 0.5.1, embassy-net 0.9.1, heapless 0.9.3,
  embedded-graphics 0.8.2, lcd-async 0.1.3; examples' oxivgl → **0.6.1** /
  oxivgl-sys → **0.2.4** (exact-pinned as a matched pair — 0.2.3+ adds
  `oxivgl_render_scratch_*` C hooks whose Rust side is in oxivgl 0.6.x, so a
  floating `^` drifts them apart and the lvgl example fails to link). Held back
  by the esp stack: allocator-api2 0.3 (esp-alloc's `Allocator` impl),
  trouble-host 0.6 (esp-radio bt-hci 0.8), and fixed 1.29 (embedded-graphics
  `az ~1.2`).

## [0.4.1] - 2026-07-20

Additive release — new BSP surfaces for SD bring-up, private PSRAM, and heap
introspection. No breaking changes.

### Added

- **`board::spi2::finish_sd` — the BSP owns full SD bring-up** (#46). One call
  runs the mandatory ≥74-clock SD power-up idle on the still-exclusive DMA bus,
  brings the display up unconditionally, restores the CoreS3 GPIO35 MISO/DC mux,
  and returns a presence-resolved `PreparedCard<CS>` — the app supplies only its
  SD driver (`SdSpi::new(prepared.into_inner())`) plus retry/degrade policy, no
  pre-init loop. `finish` remains the lower-level primitive.
  - **`CardPresence { Detect, ForceAbsent }`** — a general force-degrade control
    (no HIL vocabulary on the surface). `ForceAbsent` freezes the card
    chip-select so `SdSpi::init()` fails authentically, reaching the same
    absent-card degrade path with a card physically inserted.
  - **`PresenceCs<CS>`** — the runtime frozen-CS `OutputPin` wrapper it carries
    inside `PreparedCard`. Publish-safe: the idle uses `SpiDmaBus`'s inherent
    write, not the (git-only) `sdspi` fork.
- **`mem::psram_split(psram, reserve)` — private PSRAM region** (#47). Split
  *mapping* PSRAM from *registering* it globally: carve a private, exclusive,
  contiguous region for a foreign allocator (e.g. LVGL's TLSF via
  `lv_mem_add_pool`) and register the remainder with the global heap. Returns
  `Result<PsramSplit, PsramSplitError>` with
  `PsramSplit { private: &'static mut [MaybeUninit<u8>], global_free }`;
  carve-from-base (large-aligned, no `unsafe` at the call site), `reserve: None`
  = all-private. `PsramSplitError::{NotMapped, TooSmall { available }}`.
  `init_psram_heap` is unchanged and kept alongside.
- **`mem::internal_free()` / `mem::external_free()`** (#49) — `O(1)`
  heap-headroom readouts (global internal-DRAM / external-PSRAM free bytes)
  without a binary spelling out `esp_alloc`.

### Examples

- **`lvgl` demo: LVGL assertions no longer spin forever** (#57). `lv_conf.h`
  routed `LV_ASSERT_HANDLER` to LVGL's default `while(1);`, so a failed
  `lv_malloc` / NULL object became a silent indefinite spin. It now calls a
  `demos_lv_assert_handler` that `panic!`s → a loud `[PANIC]` (esp-backtrace +
  halt, RWDT recovers), with LVGL's `LV_LOG_ERROR` (expr/file/line) directly
  above it.
- **`sd` demo: free space via `free_clusters_hint()`** (#50) — no full-FAT scan;
  drops the `SHOW_FREE_SPACE` stopgap (bumps the `embedded-fatfs` fork rev).

## [0.4.0] - 2026-06-14

BSP code absorbed from the `alternator-regulator` application (its `altreg-*`
board crates), toward the goal of applications containing only task wiring.
Relicensed from GPL-3.0-only to MIT OR Apache-2.0 on import (same copyright
holder).

### Added

- **BSP↔binary boundary — the binary collapses to an entry shell.** New surfaces
  so per-board boilerplate lives here, not in the application:
  - **`mem::init_heap(HeapProfile, Option<PSRAM>)`** (new `heap` feature, implied
    by `psram`): the BSP owns the global heap — declares the esp-alloc DRAM
    regions for `HeapProfile::{Default,Lvgl,Coex}` (HIL-proven per-board sizes),
    so a binary never calls `esp_alloc::heap_allocator!`. (`heap` pulls
    `esp-bootloader-esp-idf` for the reclaimed-ROM region.)
  - **`io::console::install(spawner, Config)`** (#31): one-call logging — register
    the `log` backend + (when `Config::serial` is `Some`) bring up the chip
    transport (UART0 / USB-Serial-JTAG CDC) and spawn the drain; returns
    `Console { rx }` for `serial_cmd` (log TX + command RX on one port, no probe).
    Plus `console::markers` (`PANIC`/`CONSOLE_DROP`/`PREV_PANIC`, a stable HIL
    contract) and the new **`console-serial`** feature (off = R9 production
    backstop: no serial symbols; panic still breadcrumbs + halts).
  - **RTC panic breadcrumb** (#31 R8): `console::on_panic` records `{location,
    reason-digest}` to RTC-fast persistent RAM before halt; `take_panic_breadcrumb()`
    reads it once at boot — a crash survives the RWDT reset and is reported as the
    `PREV_PANIC` line, identical on both targets.
  - **`panic-handler` feature** + **`app_desc!` macro**: the BSP exports
    `#[panic_handler]` (→ `on_panic`) and wraps the esp-idf app descriptor, so a
    binary opts in instead of hand-rolling them.
  - **`board::run_app_core`** (new `multicore` feature, pulls `esp-rtos`): parks
    + starts the APP core on an `InterruptExecutor` (encapsulating the `park_core`
    JTAG-reset workaround), running a caller closure with the `SendSpawner`.
  - **`io::InputCaps` + `io::input_caps()`**: the board's input model (`Keypad`
    vs `Pointer`), so a UI installs the matching indev without hardcoding the
    board. `ButtonEvent` affirmed as a positional, app-vocabulary-free contract.
- **`board::display`** (new `display` feature, pulls `lcd-async`): ILI9342C
  panel bring-up shared by both boards — `init_ili9342c` (CoreS3, no reset
  pin / AW9523B + SPI SoftReset) and `init_ili9342c_with_reset` (Fire27),
  `SCREEN_W`/`SCREEN_H`, the `Ili9342c` type alias. De-dupes the builder
  config previously copied in every example and in the application.
- **`board::spi2`**: the shared SPI2 display + SD-card bus. Per-board
  `Spi2Resources` (pins + DMA channel) → `into_parts(dma_rx, dma_tx)` →
  `Spi2Parts::finish(card_cs)` which shares the bus, initialises the display
  (unconditionally — a dead/absent SD card must never cost the UI) and returns
  a generic-CS SD `SpiDevice`. The SD *driver* stays with the app (`sdspi` is
  not on crates.io); the module docs spell out the bounded-retry pre-init
  pattern and the chip-specific display/SD join-order asymmetry.
- **`board::cores3::Gpio35Dc` + `gpio35_disable_output`**: register-level
  GPIO35 MISO/DC muxing (the CoreS3 shares GPIO35 between SPI2 MISO and
  display DC). `Spi2Parts::finish` re-muxes to MISO after display init — the
  ordering that otherwise costs an `sd_card.init()` that never completes.
- **`board::cores3::Board` / `board::fire27::Board`** (`Board::split`): the
  boards' pin wiring as data — SPI2/display/SD pins, the internal I2C0 bus
  (hardened config: 400 kHz, `BusTimeout::BusCycles(20)`, and on the S3 a
  25 ms transaction `SoftwareTimeout` — the HIL-proven fix for the
  stuck-transaction `yield_now` spin that wedges an `InterruptExecutor`),
  buttons, radio, UART0/USB-Serial-JTAG, the SK6812 LED pin, PSRAM, free M5-Bus
  pins, and `board::SystemResources` (timers, SW interrupts, `CPU_CTRL`,
  `LPWR`). Plus `board::init()` (esp-hal at max CPU clock; heap stays with the
  app).
- **`board::spi2::Spi2Resources::into_display_only`** (feature `display`): the
  display on the descriptor-backed `SpiDmaBus` with **no** SD path, for DMA
  display-only apps (the `lvgl` example). DC is a plain `Output` on both boards
  (on CoreS3 a configured output routes GPIO35's pad, unlike `Gpio35Dc` which
  needs `with_miso`). Returns a `DisplayBus { display, backlight (Fire27) }`.
- **`io::buttons`**: unified `ButtonEvent`/`ButtonId`/`ButtonAction` input
  events for the whole Core family, plus (new `buttons` feature, pulls
  `async-button`) the Fire27 front-panel A/B/C driver
  (`ButtonResources::into_buttons` → `Buttons::next_event`).
- **`io::touch_buttons`**: CoreS3 touch→button emulation over `ft6336u` —
  three zones in the bottom strip, short/multi-tap/long-press state machine,
  emitting the same `ButtonEvent` as the physical buttons.
- **`io::watchdog::watchdog_feed_loop`**: RWDT hardware-reset backstop, armed
  and fed from the executor whose wedging it guards.
- **`must_spawn!`**: replacement for embassy-executor's `Spawner::must_spawn`
  (dropped in 0.10), panicking with call-site context.

### Changed — breaking

- `board::cores3` is now gated behind the `cores3` feature (it was
  unconditionally public; using it from a `fire27` build was meaningless).

### Examples

- **Unified the per-board example crates into one `examples/demos` crate.**
  The board is selected by a `fire27` (default) / `cores3` cargo feature; each
  bin (`display`, `i2c_scan`, `m5go`, `wifi_sta`, `onewire`, `lvgl`, `coex`)
  builds for both boards from a single source, leaning on `Board::split` +
  `board::display` + the `io` loops, with the per-board glue concentrated in
  `examples/demos/src/board.rs`. The LVGL bin's `main` is now ~50 lines (the
  flush glue / view / keypad indev moved to `examples/demos/src/ui/`).
  `examples/common` (pure chip-agnostic drawing helpers) is unchanged. Build
  per board: `cargo build -p demos --bin <name>` (Fire27, default) or add
  `--no-default-features --features cores3 --target xtensa-esp32s3-none-elf`;
  `lvgl`/`coex` are gated by `required-features`, `onewire` is Fire27-only.
  Each bin keeps its panic-handler / `esp_app_desc!()` top-matter inline (no
  macro), so it reads as a self-contained, copy-pasteable starting point. The
  shared esp-hal-fork example deps are hoisted into `[workspace.dependencies]`.
- The `coex` bin is gated by `required-features = ["coex"]` and built on its
  own (`--bin coex --features coex`): esp-radio's coexist blob is a crate-global
  link dependency that only the BLE-initialising bin can satisfy, so the `coex`
  feature must stay off while the non-BLE bins are built (it is, for every
  normal `cargo build` / `--workspace` / `-p demos` invocation).
- Input is now unified across boards via the new `io::buttons::ButtonEvent`
  (the `display` bin reads Fire27 buttons / CoreS3 touch through one loop), and
  logging is unified on the `log` facade (CoreS3 over RTT at `Info`).
- The sensor/peripheral demos (`i2c_scan`, `m5go`, `wifi_sta`, `coex`,
  `onewire`) now render through one shared `common::draw_panel` (a cyan
  board/title header + body lines) so they look alike, and each shows
  everything it has **on screen** — not the console alone: `onewire` gained a
  display (sensor count + per-sensor ROM + °C), and `wifi_sta`/`coex` now show
  the nearby-AP scan (via the shared `demos::net`, which also de-dups the
  `net_demo` task). The `display`/`lvgl` demos keep their own rendering.
- The `display` bin is now a clear input-capabilities demo: a per-position
  (Left/Center/Right) readout of the last `ButtonEvent` — `tap` / `tap x2…` /
  `HELD (long)` — so multi-tap count and long-press are legible, identical on
  Fire27 buttons and CoreS3 touch.
- The `lvgl` bin is now **interactive**: three focusable LVGL buttons navigated
  from the front panel (`PREV`/`NEXT`/`ENTER`), identical on both boards — the
  unified `ButtonEvent` is mapped to LVGL keys feeding an oxivgl `KeypadState`
  (`run_app_nav_keypad_events`), routed to the view's focus group. Per-board
  bring-up is `board::lvgl_bringup` (display + the right `Input`); on CoreS3 the
  one I2C bus resets the panel **and** drives touch. The previous raw-FFI
  Fire27-only keypad glue is replaced by this unified, both-boards path.
  **CoreS3 additionally runs a direct-touch POINTER indev** (oxivgl 0.5
  `PointerIndev`, fed by an async FT6336U poll task bridging the I2C read into
  the indev's sync read callback), so on-screen widgets can be tapped by
  coordinate — the bottom-strip keys stay on the BSP **button API**
  (`TouchButtons`, multi-tap / long-press, `multi_tap_ms` tuned to 150 ms for
  snappy focus-nav) *and* taps land anywhere on screen (#32 I3).
- Adopted **oxivgl 0.5** (from crates.io, no git pin) for the `lvgl` bin:
  `KeypadIndev`/`KeypadState` + the new `PointerIndev`/`PointerState`.
- New `sd` bin (gated by `--features sd`): the one demo exercising
  `board::spi2::finish()` — the display + SD shared bus including the CoreS3
  GPIO35 MISO/DC mux. Mounts the FAT filesystem **read-only** and lists the root
  dir; handles MBR-partitioned cards (mounts the first FAT partition via a
  `StreamSlice`) and superfloppies (FAT at sector 0). Pulls the `sdspi` +
  `embedded-fatfs` fork (not on crates.io → example-only).
- Bumped the **esp-hal fork pin** (example/local builds only — the published
  library uses stock esp-hal `=1.1.1`) to `b7a4c74a`, which scopes the I2C
  NACK-recovery skip to ESP32 so the ESP32-S3 shared I2C bus recovers after a
  NACK again. Without it a single NACK (e.g. probing an absent address) could
  poison every later transaction on the S3 bus — manifesting as dead CoreS3
  touch or a cold-boot black screen. Surfaced downstream in `alternator-regulator`.

## [0.3.1]

### Changed

- Bumped **esp-hal 1.1.0 → 1.1.1** — both the published stock dependency and
  the esp-hal fork (rebased onto the 1.1.1 release as the `local-1.1.1` branch,
  pinned by rev in `[patch.crates-io]`). `esp-rom-sys` stays pinned to `=0.1.4`.
  Verified: lib builds on stock crates.io esp-hal 1.1.1; both boards run on HW.

### Examples

- The `examples/lvgl` (oxivgl/LVGL) example now builds and runs on **CoreS3**
  (ESP32-S3) as well as Fire27 — GDMA flush, panel reset via AW9523B + backlight
  via AXP2101, and RTT logging (`rtt-target`) + `panic-halt` over USB-Serial-JTAG
  (esp-println/esp-backtrace conflict with USB-Serial-JTAG). The RTT logger runs
  at **Info**, not Trace: oxivgl's per-frame DEBUG stream otherwise floods the
  RTT buffer and, with no debugger draining it, back-pressures and stalls the
  render loop — HIL-confirmed. At Info the demo runs standalone.

## [0.3.0]

### Added

- `driver::onewire` — async 1-Wire bus master over the RMT peripheral, vendored
  in-tree (previously the external `esp-hal-rmt-onewire` git dependency, which
  blocked publishing and lagged esp-hal releases). Public API: `OneWire`,
  `Address`, `Search`, `crc8`. New `search-masks` feature gates the masked ROM
  search.
- `driver::ds18b20::Ds18b20Driver::rescan` — re-enumerate the bus on demand.

### Changed — breaking

- Renamed `driver::ds16b20` → `driver::ds18b20` and `Ds16b20Driver` →
  `Ds18b20Driver` (the sensor is the DS**18**B20; "16" was a typo).
- `Address` is now `driver::onewire::Address` (previously surfaced from the
  external crate). Its `Display`/`LowerHex` render byte-wise, family-code first
  and zero-padded. `onewire::Error`, `SearchError`, and `ds18b20::Error` are
  reworked onto `thiserror_no_std` with `#[from]` sources.
- DS18B20 ROM addresses are now enumerated once and cached (no sensor hot-plug
  assumed); reads address each sensor by `Match ROM` instead of re-running the
  full ROM search every cycle.

### Fixed

- DS18B20 reads wait for the temperature conversion to complete before reading,
  so the first read no longer returns the stale 85 °C power-up value.
- Each device's full 9-byte scratchpad is read and CRC-8 validated, and ROM
  addresses are CRC-checked during enumeration. A CRC-bad or dropped sensor is
  warned and omitted rather than aborting the whole read; its address stays
  cached and is retried on the next cycle.
- Replaced the 1-Wire RX-timeout hack (a second TX "delay pulse" plus
  undocumented select-polling) with a software-timer-bounded `join(rx, tx)`.

### Examples

- Replaced the monolithic kitchen-sink demos with focused per-topic example
  bins (`display`, `i2c_scan`, `m5go`, `wifi_sta`, `coex`) for both boards,
  behind a shared `examples/common` crate.
- Added `examples/lvgl` — an oxivgl (LVGL) display example for Fire27.
- Added `examples/fire27/src/bin/onewire.rs` — a DS18B20 1-Wire HIL example
  (2× sensors on Port B / G26).

### Packaging

- The library now depends on **stock crates.io** versions of the esp-hal family
  (`esp-hal 1.1.0`, `esp-radio 0.18.0`, `esp-sync`, `esp-alloc`) — **no git
  dependencies** — so it is publishable to crates.io. `esp-rom-sys` is pinned to
  `=0.1.4` (esp-hal 1.1.0 calls a 0.1.4 API but only constrains it to `~0.1`).
- Local and example builds are redirected to the emobotics esp-hal fork (ESP32
  SPI/DMA patches the examples need) via `[patch.crates-io]`, pinned to a commit
  rev for reproducibility; `cargo publish` ignores it. The LVGL example's
  `oxivgl` deps use the crates.io release. See the README "Dependencies" section.

### Hardware notes

- 1-Wire / DS18B20 hardening verified on Fire27 with 2× DS18B20 on Port B
  (G26): correct reads across cycles, and a bus disconnect degrades gracefully
  (per-sensor warn + omit, addresses retained, no hang or crash).

## [0.2.0]

### Added — M5GO Battery Bottom support

- `driver::sk6812` — SK6812/WS2812 addressable-RGB LED-strip driver over the RMT
  peripheral (`Sk6812Driver::write`/`fill`, `Rgb`). Drives the bottom's two
  5-LED bars on the M-Bus LED line (physical pin 23): **GPIO15** on Fire27,
  **GPIO13** on CoreS3.
- `driver::ip5306` — IP5306 battery gauge over I2C `0x75` (`battery_level`,
  `is_charging`, `is_charge_full`, `present`). This is the battery path on the
  Fire (and the PMIC-less classic Core); CoreS3 uses the AXP2101 instead.
- `driver::axp2101::Axp2101Driver::enable_battery_adc` — enable the VBAT ADC so
  `battery_voltage_mv` returns a live reading (CoreS3 battery path).
- `driver::aw9523b::Aw9523bDriver::enable_bus_5v` — enable the CoreS3 M-Bus /
  Grove **5 V output** (asserts `BOOST_EN` P1_7 + `BUS_OUT_EN` P0_1 high, after
  switching P0 to push-pull). Required to power an attached bottom's LEDs on
  CoreS3. New public pin/register constants: `P0_BUS_OUT_EN`, `P1_BOOST_EN`.
- Both board examples drive a colour-wheel on the bars and show the battery
  reading on the LCD (Fire27: IP5306 %; CoreS3: AXP2101 mV).

### Fixed

- `driver::aw9523b` docs: `BUS_OUT_EN` (P0_1) is **active-HIGH**, not active-LOW
  as a prior revision stated (verified against the CoreS3 schematic and
  M5Unified). Documented that Port 0 is open-drain by default and must be set to
  push-pull before it can drive the enable high.

### Hardware notes

- All additions verified on hardware: Fire27 (both LED bars + IP5306 battery %),
  CoreS3 (LED bars via the AW9523 5 V enable + AXP2101 battery mV).
- The A014 "Base M5GO Bottom" is a classic-Core part: it cannot sustain a CoreS3
  on battery (the board powers down on unplug), so on CoreS3 it runs on USB with
  the bottom's battery present. The CoreS3-matched bottom is the Bottom3 (A014-D).

## [0.1.0]

- Initial board support: shared I2C bus, drivers (`pcnt`, `pps`, `ds16b20`,
  `aw9523b`, `axp2101`, `ft6336u`, `radio`), reusable async IO task loops,
  CoreS3 display bring-up, WiFi/BLE coexistence.
