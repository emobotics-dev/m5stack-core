# Changelog

All notable changes to this crate are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0]

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
