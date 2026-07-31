# m5stack-core

Board support crate for **M5Stack Fire27** (ESP32) and **CoreS3** (ESP32-S3).

Provides chip-agnostic drivers, shared I2C bus, and reusable async IO task loops with `fn(...)` callbacks.

## Documentation

**API docs: <https://emobotics-dev.github.io/m5stack-core>** — one rustdoc tree
per board, built from `master` with all optional features on.

The docs.rs build for this crate fails, and cannot be made to pass: docs.rs runs
the stock rust-lang toolchain, which has no Xtensa backend, and both supported
boards are Xtensa (ESP32 / ESP32-S3). The link above is the self-hosted
substitute, the same approach `esp-idf-hal` takes.

## Features

| Feature | Target | Chip |
|---------|--------|------|
| `fire27` | `xtensa-esp32-none-elf` | ESP32 |
| `cores3` | `xtensa-esp32s3-none-elf` | ESP32-S3 |

Exactly one board feature must be enabled. Orthogonal opt-ins:

| Feature | Enables | Pulls in |
|---------|---------|----------|
| `display` | `board::display` (ILI9342C bring-up) + `board::spi2` device construction | `lcd-async`, `embassy-embedded-hal` |
| `buttons` | `io::buttons::Buttons` — debounced Fire27 front-panel driver | `async-button` |
| `app-desc` | `app_desc!` + `app_elf_sha256()` — the esp-idf application descriptor (**implied by `heap`**) | `esp-bootloader-esp-idf` |
| `identity` | enforced firmware build identity — `app_desc!()`'s version field becomes the consumer's git commit + dirty state (**implies `app-desc`**) | (via `app-desc`) |
| `heap` | `mem::init_heap` — BSP owns the global heap (DRAM regions + HIL-proven per-board sizes) | `esp-alloc`, (`app-desc`) |
| `psram` | external-PSRAM region + checked `psram_box`/`psram_vec` (**implies `heap`**) | (via `heap`) |
| `console-serial` | the `io::console` serial transport (UART0 / USB-Serial-JTAG); **off** = R9 production backstop (no serial symbols, panic still breadcrumbs + halts) | — |
| `panic-handler` | BSP-provided `#[panic_handler]` → `io::console::on_panic` (opt in, or supply your own) | — |
| `multicore` | `board::run_app_core` — park + start the APP core on an esp-rtos `InterruptExecutor` | `esp-rtos` |
| `serial-cmd` | `io::serial_cmd` HIL command endpoint | — |
| `search-masks` | masked 1-Wire ROM search | — |
| `ble`/`wifi`/`wifi-sta`/`coex` | radio (see the radio section) | `embassy-net` (sta) |

A binary built on this BSP can be a thin entry shell: `mem::init_heap` owns the
heap, `io::console::install` owns logging + the panic breadcrumb, the
`panic-handler` feature provides `#[panic_handler]`, `app_desc!` the descriptor,
`board::run_app_core` the second core, and `io::input_caps()` the input model —
so per-board boilerplate stays in the BSP, not the application.

## Modules

### Drivers (`driver::`)

| Module | Description |
|--------|-------------|
| `pcnt` | Pulse counter wrapper for RPM sensing (`PcntDriver`) |
| `pps` | Programmable Power Supply I2C driver (0x35) — voltage, current, temperature |
| `ds18b20` | 1-Wire temperature sensor via RMT (chip-specific RMT channel selection) |
| `aw9523b` | I2C GPIO expander (CoreS3, 0x58) — LCD/touch reset pulses, M-Bus 5 V enable (`enable_bus_5v`) |
| `axp2101` | PMIC (CoreS3, 0x34) — backlight voltage, battery ADC, VBUS detection |
| `ft6336u` | Capacitive touch controller (0x38) — stateless `read_touch()` |
| `ip5306` | Fire27 / classic-Core battery gauge (I2C 0x75) — coarse battery %, charge / charge-full flags (CoreS3 uses `axp2101` instead) |
| `sk6812` | M5GO Battery Bottom RGB LED bars (SK6812/WS2812 via RMT) — `write()` a colour frame |
| `radio` | Shared radio (`esp-radio`). Parent of `radio::ble` (BLE `BleConnector`) and `radio::wifi` (WiFi controller + STA stack) — see [WiFi + BLE](#radio-wifi--ble-driverradio) |

#### M5GO Battery Bottom

The M5GO Battery Bottom plugs into the M-Bus and adds a LiPo cell and ten RGB
LEDs (the A014 "Base M5GO Bottom" uses SK6812; the CoreS3-matched A014-D
"Bottom3" uses WS2812 — the RMT driver drives both). The LED data line sits on a
fixed *physical* M-Bus pin (pin 23) that maps to a **different GPIO per core**:

| | Fire27 (ESP32) | CoreS3 (ESP32-S3) |
|---|---|---|
| RGB LEDs (RMT, M-Bus pin 23) | `GPIO15` | `GPIO13` |
| Battery | `ip5306` @ I2C `0x75` (onboard) | `axp2101` @ I2C `0x34` (onboard) |
| LED 5 V rail | always present | **must be enabled** via `aw9523b` |

The LEDs are a one-wire NRZ protocol (RMT), **not** I2C. **Battery management
differs by board:** the Fire (and the PMIC-less classic Core, via the bottom's
own IP5306) report through an **IP5306 at `0x75`**; the **CoreS3 manages the
cell — including the bottom's battery — with its onboard AXP2101 at `0x34`**, so
a bottom's IP5306 does not appear on the CoreS3 I2C bus.

**CoreS3 — powering the LEDs.** The bottom's LEDs are fed from the CoreS3 M-Bus
5 V rail, which is **off by default** and gated by the AW9523 expander. Call
[`Aw9523bDriver::enable_bus_5v`] to bring it up — it asserts `BOOST_EN` (P1_7) and
`BUS_OUT_EN` (P0_1) **high** (both active-HIGH; P0 must be switched to push-pull
first, as it is open-drain by default). Guard it as M5Unified does: only enable
when a **battery is present or USB is absent** (the bus output shares the USB
VBUS node, so enabling it with no battery on USB contends the rail). Note the
A014 bottom is a *classic-Core* part — it can't sustain the CoreS3 on battery
(the board powers down on unplug), so in practice it runs on USB with the
bottom's battery present. The CoreS3-matched bottom is the **Bottom3 (A014-D)**.

Both examples drive a colour-wheel animation on the bars and show the battery
reading on the LCD (Fire27: IP5306 %; CoreS3: AXP2101 mV).

### Radio: WiFi + BLE (`driver::radio`)

The on-package radio is shared between **BLE** and **WiFi**, modelled as
sub-modules of `driver::radio` and gated by cargo features so a binary only
compiles (and pays the RAM for) the radios it uses. All WiFi is `async`.

| Feature    | Enables | Pulls in |
|------------|---------|----------|
| `ble`      | `radio::ble::BleRadio` — `BleConnector` (HCI transport) | — |
| `wifi`     | `radio::wifi::Wifi` — controller + `scan()` (no IP stack) | — |
| `wifi-sta` | `Wifi::into_sta()` → `embassy_net::Stack` (STA + DHCP/static) | `embassy-net` |
| `wifi-ap`  | reserved for AP mode (not yet implemented) | `embassy-net` |
| `coex`     | run BLE and WiFi simultaneously (implies `wifi` + `ble`) | extra RAM |

The BSP exposes only the BLE *controller* (`BleConnector`); the BLE host stack
(`trouble-host`) is an application dependency — see the cores3 coex example. On
this esp-hal 1.1 line `BleConnector` speaks **bt-hci 0.8**, so pair it with
`trouble-host` 0.6 (the older 0.5 / bt-hci 0.6 line won't bind). `coex` costs
significant heap (~96 KB reclaimed on ESP32); enable it only when both radios run
together. `esp_rtos::start(..)` must run before any radio is created.

**STA bring-up.** The BSP owns the controller + net runner; the app supplies a
`seed` (from its own TRNG — the BSP leaves `RNG`/`ADC1` free) and a
`static`-lifetime `StackResources`, then spawns one task:

```rust
use m5stack_core::driver::radio::wifi::{self, AuthenticationMethod, IpSetup, StaCredentials};

let wifi = wifi::Wifi::new(peripherals.WIFI)?;
let (stack, control, runner) = wifi.into_sta(
    StaCredentials { ssid, password, auth: AuthenticationMethod::Wpa2Personal },
    IpSetup::Dhcp,                                       // or IpSetup::Static(..)
    seed,
    make_static!(embassy_net::StackResources::<3>::new()),
)?;
spawner.spawn(wifi::wifi_task(runner).unwrap());        // manages assoc + runs the stack
stack.wait_config_up().await;                           // IP acquired
let aps = control.scan().await?;                        // scan while associated
```

`wifi_task` is the single owner of the controller: it auto-connects, reconnects
on link loss, and serves `WifiControl` commands (`scan`/`connect`/`disconnect`)
so scanning never races association. Scan-only firmware can skip `wifi-sta` and
call `Wifi::scan()` directly. AP mode is a planned extension point (`into_ap` +
`Config::AccessPoint`).

Variant note: the esp-radio WiFi API is identical on both chips; only RAM
differs. The `ControllerConfig` RX buffers are trimmed on Fire27 (ESP32). **Fire27
cannot DMA from PSRAM** — keep `StackResources`/socket buffers in internal RAM.

### IO Tasks (`io::`)

Async task loops using `embassy_time::Ticker` with `fn(...)` callbacks for decoupled integration.

| Module | Loop interval | Callback |
|--------|---------------|----------|
| `rpm` | configurable | `fn(f32)` — RPM value |
| `pps` | 500 ms | `fn(&PpsReadings)` + `fn() -> PpsSetpoint` |
| `ow_temp` | 3 s | `fn(&[(u64, f32)])` — address/temperature pairs |
| `shared_i2c` | — | `SharedI2cBus` async mutex for multi-task I2C access |

#### Input (`io::buttons`, `io::touch_buttons`)

Both input flavours emit the same `ButtonEvent { id: Left/Center/Right, action:
Short(taps)/Long }`, so an application maps input in one place for both boards:

- **Fire27** (feature `buttons`): `ButtonResources::into_buttons()` →
  `Buttons::next_event().await` — debounced `async-button` over the physical
  A/B/C keys (GPIO39/38/37).
- **CoreS3**: `TouchButtons::new(i2c, TouchButtonsConfig::default())` →
  `next_event().await` — the FT6336U bottom strip split into three zones, with
  short/multi-tap/long-press detection.

Both drivers count consecutive taps, which delays every single press by the
multi-tap window (~350 ms Fire27 / 300 ms CoreS3). Latency-sensitive input (an
encoder turn/click) wants each press *immediately*. The choice is one unified,
cross-driver type — `io::buttons::ButtonTiming { long_press_ms, multi_tap_ms }`
(`multi_tap_ms: 0` disables counting) — applied to either board the same way:

- **Fire27**: `ButtonResources::into_buttons_with_timing(timing)`
- **CoreS3**: `TouchButtons::with_timing(i2c, timing)` (or
  `TouchButtonsConfig::with_timing(timing)`)

with presets `ButtonTiming::multi_tap()` (count taps) and
`ButtonTiming::immediate_single_press()` (instant). The legacy `into_buttons()`
and `TouchButtons::new(i2c, TouchButtonsConfig::default())` keep each board's
original timings. Counting still works downstream: an accumulator that sums
immediate `Short(1)` events preserves "double-tap = 2 steps" without the delay.

#### Watchdog (`io::watchdog`)

`watchdog_feed_loop(rtc, timeout_secs, feed_every_secs)` arms the RWDT
(hardware reset on timeout) and feeds it from the calling executor — the
backstop for a fully wedged executor.

### Board bring-up (`board::`)

- `board::init()` — esp-hal at max CPU clock (heap setup stays with the app).
- `board::fire27::Board::split(peripherals)` / `board::cores3::Board::split(...)`
  — the boards' pin wiring as data: `spi2` (display+SD bus resources), `i2c0`
  (hardened config; returned *blocking* so the app binds the IRQ core via
  `into_async()`), buttons, radio, console peripherals, free M5-Bus pins, and
  `SystemResources` (timers, SW interrupts, `CPU_CTRL`, `LPWR`).
- `board::display` (feature `display`) — ILI9342C panel init shared by both
  boards; `board::spi2` — the shared display+SD bus with the bring-up ordering
  encoded (display first, bounded SD attempts, CoreS3 GPIO35 MISO/DC re-mux).
  - `Spi2Parts::finish_sd(card_cs, presence)` is the one-call SD bring-up: it
    runs the mandatory ≥74-clock SD power-up idle on the still-exclusive bus,
    brings the display up unconditionally, and returns a presence-resolved
    `PreparedCard<CS>` — the app supplies only its SD driver
    (`SdSpi::new(prepared.into_inner())`) plus retry/degrade policy. `finish`
    remains the lower-level primitive. `CardPresence::{Detect, ForceAbsent}`
    forces the absent-card degrade path with a card physically inserted (HIL),
    via the `PresenceCs<CS>` frozen chip-select carried inside `PreparedCard`.
  - The SD-card *driver* (`sdspi`) stays an application dependency until it is
    published on crates.io; see the `board::spi2` module docs for the wiring
    pattern.
- `board::cores3::power_display_reset` — AW9523B LCD/touch reset pulses +
  AXP2101 backlight rail over the shared I2C bus.

### Memory (`mem::`)

The BSP owns the global heap (feature **`heap`**, implied by `psram`): one call,
`mem::init_heap(profile)`, declares the `esp-alloc` DRAM regions for a
`HeapProfile` (the HIL-proven per-board sizes) — so a binary never spells out
`esp_alloc::heap_allocator!`. **`init_heap` never touches PSRAM** — that's a
separate, deliberate decision, made below:

```rust
use m5stack_core::mem::{self, HeapProfile};

mem::init_heap(HeapProfile::Default); // reclaimed + plain DRAM
mem::init_heap(HeapProfile::Lvgl);    // reclaimed-ROM only
mem::init_heap(HeapProfile::Coex);    // more controller heap
```

PSRAM specifics, behind the **`psram`** feature. Both boards have external SPI
PSRAM (Fire27 ~4 MB, CoreS3 ~8 MB). Getting PSRAM into the global heap is
opt-in and explicit — no function does it as a side effect of anything else,
because once a region carries external capability, *every* plain
`alloc::vec!` / `Box` / `String` in the whole crate graph (not just your own
code) becomes eligible to silently spill into it once internal DRAM runs out.

```rust
use m5stack_core::mem;

// The default: maps PSRAM as one private region. Never touches the global
// heap — nothing here is ever reachable by a plain allocation.
let psram = mem::psram_map(peripherals.PSRAM);
```

For **deliberate** global exposure — so the checked `psram_box` / `psram_vec`
helpers (atomic-bearing types rejected at compile time) can reach part of
PSRAM — use `mem::psram_split` instead, which splits *mapping* from
*registering*:

```rust
use m5stack_core::mem;

// Carve 512 KiB private off the base; register the remainder with the global heap.
let split = mem::psram_split(peripherals.PSRAM, 512 * 1024)?;
// `split.private: &'static mut [MaybeUninit<u8>]` — no unsafe at the call site,
// base is large-aligned. `split.global_free` = external bytes now in the heap.

let mut big = mem::psram_vec::<u8>(512 * 1024);  // in PSRAM; atomics rejected at compile time
let scratch = mem::psram_box([0u32; 1024]);      // in PSRAM
let dma = mem::dma_buffer(4 * 1024);             // in internal DRAM; DMA-safe
```

`reserve: 0` is the maximal-exposure case — an empty private slice, all PSRAM
global — sound but rarely what you want; reach for `psram_map` if you don't
need any global exposure at all. `psram_split` returns
`Result<PsramSplit, PsramSplitError>` (`NotMapped`, or `TooSmall { available }`).

The raw markers `ExternalMemory` / `InternalMemory` are still re-exported for
direct `allocator_api2` use, but they skip the atomic check — use them only when
you know what's going into PSRAM.

For headroom checks, `mem::internal_free()` / `mem::external_free()` return the
global heap's free internal-DRAM / external-PSRAM bytes (`O(1)`) without a binary
touching `esp_alloc`.

The three hardware caveats are now mostly **enforced** rather than just
documented:

| Caveat | Enforcement |
|--------|-------------|
| No `Atomic*` in PSRAM (broken atomic RMW on ESP32/-S3) | **Compile-time** — `psram_box`/`psram_vec` bound `T: PsramSafe`, a `Send`/`Sync`-style auto trait with negative impls for the atomics. A type embedding an atomic (directly or transitively) won't compile. |
| ESP32 (Fire27) can't DMA out of PSRAM | **Runtime `debug_assert`** — `mem::assert_dma_capable(buf)` rejects a PSRAM-backed buffer on Fire27 (no-op on CoreS3, which *can* DMA from PSRAM). Use `mem::dma_buffer(n)` to get an internal-DRAM buffer. |
| PSRAM timing needs `opt-level` > 0 | **Build-time** — `build.rs` fails the build if the `psram` feature is on at `opt-level = 0`. Both profiles already use `"s"`. |

`PsramSafe` requires the `esp` toolchain's `auto_traits` + `negative_impls`
(enabled only when `psram` is on). No esp-hal Cargo feature is required — PSRAM
itself is available under the already-enabled `unstable` feature.

### Firmware identity (`app_desc!`)

`app_desc!()` (feature `app-desc`, implied by `heap`) emits the esp-idf
application descriptor — a fixed, ELF-mapped struct (project name, version,
build time, and `app_elf_sha256()`, the built image's own content hash). A
host can read it straight off flash without booting the board, which is what
makes a "skip the flash if this board already carries the intended image"
workflow possible. Invoke it once, in the binary (not the BSP), so it captures
the *application's* `CARGO_PKG_VERSION`:

```rust
m5stack_core::app_desc!();
```

By default the version field is plain `CARGO_PKG_VERSION`. The **`identity`**
feature (implies `app-desc`) makes it a build-time git descriptor instead —
same call site, no application-code change — and turns "forgot to wire this
up" into a compile error rather than a silent gap:

1. Add [`m5stack-core-build`](m5stack-core-build) as a `[build-dependencies]` entry.
2. Call it once from your own `build.rs`:
   ```rust
   fn main() {
       m5stack_core_build::emit_identity_env();
   }
   ```
3. Enable the `identity` feature. `app_desc!()`'s call site is unchanged, but
   its expansion now requires `M5STACK_CORE_BUILD_MARK` (an 8-hex-char
   abbreviated commit hash + a `*` if the tree is dirty, e.g. `a1b2c3d4*`) —
   if step 1 or 2 was skipped, that's `env!()` failing to compile *in your own
   crate*, pointing straight at the `app_desc!()` line, not a silently-plain
   version field.

`EspAppDesc::version` is a fixed 32-byte C string (silently truncated past
that) — `m5stack-core-build` keeps the mark itself short (≤16 bytes) to leave
headroom. The mark alone becomes the whole version field; nothing prefixes it
with `CARGO_PKG_VERSION` automatically, so if you want that too, fold it into
what `m5stack-core-build` emits, or set `M5STACK_CORE_BUILD_MARK` yourself
from your own `build.rs` instead — the env var's *name* is the only contract,
not how it gets set. BSP owns the mechanism (reading the descriptor back,
enforcing the wiring); the consumer's own `build.rs` owns the content —
`m5stack-core` never inspects its own git tree, only `m5stack-core-build`
does, and only against the *calling* crate's directory.

### Serial console (`io::console`)

The **complete** async logging console for the firmware — both the target-agnostic
pipeline AND the per-target hardware. No `esp-println`/`esp-backtrace`/RTT.

- **`install(spawner, Config) -> Console`** — the one call: register the `log`
  backend at a level and (when `Config::serial` is `Some`) build the chip
  transport (UART0 @ 1 Mbaud on Fire27, USB-Serial-JTAG CDC on CoreS3) + spawn
  the drain. Returns `Console { rx }` whose RX half feeds `serial_cmd` (log TX +
  command RX share one port — no debug probe needed). `serial: None` is the R9
  production backstop. Replaces the old `init`/`setup`/`drain_task`/`enable_async`
  sequence (still public for advanced use).
- **`markers`** — `PANIC` / `CONSOLE_DROP` / `PREV_PANIC`, the **stable** strings
  the HIL crash detector greps; treat as contract.
- **`on_panic(&PanicInfo) -> !`** — writes an RTC-persistent breadcrumb
  (`{file, message digest}`) *before* a best-effort transport print, then halts
  (the RWDT recovers). Installed for you by the **`panic-handler`** feature.
- **`take_panic_breadcrumb() -> Option<PanicCrumb>`** — call once at boot, before
  `install`, and log the result: a previous-run panic survives reset and is
  reported once as the `PREV_PANIC` line — the cross-transport fault contract,
  identical on both targets.
- **`send_line(Arguments)`** — back-pressuring bulk-dump emit (the `:cat`
  read-back) and the **R10 injection point**: a control crate defines its own
  `LineSink` trait and the binary forwards to `send_line`, so the trait never
  enters the BSP.

With the **`console-serial`** feature off (R9), the transport compiles out
entirely: `log!` is a no-op into the ring, `install` only registers the backend,
and `on_panic` still breadcrumbs + halts — zero serial surface.

### Key types

```rust
// io::rpm
pub struct RpmConfig { pub loop_time_ms: u64, pub pole_pairs: f32, pub pulley_ratio: f32 }
pub fn read_rpm(pcnt: &mut PcntDriver, config: &RpmConfig) -> f32
pub async fn rpm_loop(resources: RpmResources<'static>, config: RpmConfig, on_rpm: fn(f32))

// io::pps
pub struct PpsReadings { pub voltage: f32, pub current: f32, pub temperature: f32, ... }
pub struct PpsSetpoint { pub current_limit: Option<f32>, pub voltage_limit: Option<f32>, pub enabled: Option<bool> }
pub async fn pps_loop(resources: PpsResources, on_read: fn(&PpsReadings), get_setpoint: fn() -> PpsSetpoint)

// io::ow_temp
pub async fn ow_loop(resources: OnewireResources<'static>, on_temperatures: fn(&[(u64, f32)]))
```

## Examples

All examples live in **one** crate, `examples/demos` — small, single-topic
binaries (one subsystem each) that build for **both boards**, with the board
selected by a cargo feature: `fire27` (default) or `cores3`. Each bin leans on
the BSP's `Board::split` + `board::display` + `io` loops, so the board-specific
wiring lives in the BSP and the per-board duplication is gone; the per-bin glue
that remains is concentrated in `examples/demos/src/board.rs`. Chip-agnostic
drawing helpers (colour wheel, splash/status rendering, I2C scan) stay in the
shared `examples/common` crate.

Select a board by feature and a bin with `--bin <name>`:

```bash
# Fire27 (ESP32) — the default board; default target is xtensa-esp32-none-elf
cargo +esp run --release -p demos --bin <name>

# CoreS3 (ESP32-S3)
cargo +esp run --release -p demos --bin <name> \
  --no-default-features --features cores3 --target xtensa-esp32s3-none-elf
```

| bin        | what it shows                                                  | needs |
|------------|----------------------------------------------------------------|-------|
| `display`  | unified front-panel events (Fire27 buttons / CoreS3 touch) + `input_caps()` | — |
| `i2c_scan` | I2C bus scan (0x08..0x77), addresses on LCD                     | — |
| `m5go`     | SK6812 LED colour-wheel (M-Bus pin 23) + battery readout        | M5GO bottom attached |
| `wifi_sta` | WiFi STA + DHCP + AP scan, IP on LCD                            | `WIFI_SSID`/`WIFI_PASSWORD` |
| `onewire`  | DS18B20 1-Wire temperature read over RMT (G26)                  | **Fire27 only** |
| `lvgl`     | LVGL (oxivgl) UI: 3 focusable buttons navigated by the front panel + frame counter | `--features lvgl` |
| `coex`     | `wifi_sta` plus a BLE peer-MAC scanner                          | `--bin coex --features coex`, `--release` |
| `sd`       | mount the SD card on the shared SPI2 bus + list the root dir (read-only) | `--features sd` |
| `multicore`| runs a task on the APP core via `board::run_app_core` (interleaved PRO/APP ticks) | `--features multicore` |

Notes on building:

- `WIFI_SSID`/`WIFI_PASSWORD` are read at build time; unset → WiFi is skipped
  and the display still runs. The `m5go` bin needs the M5GO Battery Bottom
  attached to do anything visible.
- `onewire` is **Fire27-only** (`required-features = ["fire27"]`); a CoreS3
  build skips it automatically. `lvgl` and `coex` are gated by
  `required-features` so a plain `cargo build -p demos` (and `--workspace`)
  skips them (and the LVGL C build).
- **Build `coex` on its own** — `cargo build -p demos --bin coex --features coex`.
  esp-radio's WiFi/BLE coexist blob is a *crate-global* link dependency that
  only the BLE-initialising `coex` bin can satisfy, so enabling `--features
  coex` while building the *other* (non-BLE) bins fails to link with a cryptic
  `btdm_rf_bb_reg_init`. Because the bin is gated by `required-features`, the
  `coex` feature stays off for every normal build (`cargo build`,
  `--workspace`, `-p demos`, per-bin) — only an explicit `--features coex`
  without `--bin coex` hits it, which is why the coex bin is always built alone.
- **The `sd` bin** is gated by `--features sd` (it pulls the `sdspi` +
  `embedded-fatfs` fork, which isn't on crates.io, so it's example-only). It's
  the one demo that exercises `board::spi2::finish_sd` — the display + SD shared
  bus, including the CoreS3 GPIO35 MISO/DC mux. It mounts **read-only** (never
  writes, so it won't touch existing logs) and handles both MBR-partitioned
  cards (mounts the first FAT partition via a `StreamSlice`) and superfloppies
  (FAT at sector 0); a dead/absent card still lets the display come up.
- **Input is unified**: both boards emit the same `ButtonEvent` (Fire27 reads
  the three buttons; CoreS3 splits the FT6336U touch strip into three zones), so
  the `display` bin's readout loop is identical. The `display` bin also queries
  `io::input_caps()` (#32) and logs the board's input model.
- **Logging goes through the BSP console** (`io::console::install`, the
  `console-serial` + `panic-handler` features) on both boards — Fire27 over
  **UART0 @ 1 Mbaud**, CoreS3 over the **probe-free USB-Serial-JTAG CDC**. No
  `esp-println`/`esp-backtrace`/RTT; the bins carry no panic boilerplate (the BSP
  provides `#[panic_handler]` + `app_desc!`). A panic is recoverable across reset
  via the RTC breadcrumb (logged once at next boot as `[bc] previous panic`).
- **The sensor/peripheral demos all render through one `common::draw_panel`**
  (a cyan board/title header + body lines) so they look alike, and each shows
  everything it has *on screen* — not just the console (`onewire` displays the
  sensor ROMs + °C; `wifi_sta`/`coex` show the nearby-AP scan). `display` and
  `lvgl` keep their own rendering.

```bash
WIFI_SSID=myssid WIFI_PASSWORD=secret cargo +esp run --release -p demos --bin wifi_sta
cargo +esp run --release -p demos --bin lvgl --features lvgl
cargo +esp run --release -p demos --bin coex --features coex
# CoreS3:
cargo +esp run --release -p demos --bin coex \
  --no-default-features --features cores3,coex --target xtensa-esp32s3-none-elf
```

GPIO — **Fire27**: I2C SDA=21/SCL=22, SPI CLK=18/MOSI=23/MISO=19, Display
CS=14/DC=27/RST=33/BL=32, Buttons=39/38/37, M5GO LEDs=15, IP5306@0x75.
**CoreS3**: I2C SDA=12/SCL=11, SPI CLK=36/MOSI=37, Display CS=3/DC=35, RST via
AW9523B, BL via AXP2101 DLDO1, M5GO LEDs=13, AXP2101@0x34.

### The `lvgl` bin

Drives the panel with **[oxivgl](https://github.com/emobotics-dev/oxivgl)**
(safe LVGL 9 bindings) instead of `embedded-graphics`, with the SPI flush on a
high-priority `InterruptExecutor` so the UI stays smooth while the main task
works. It's **interactive**: three focusable buttons that you navigate from the
front panel — `PREV`/`NEXT` move focus, `ENTER` clicks (a counter ticks up).

The input path is **identical on both boards** and is the point of the demo: the
unified `io::buttons::ButtonEvent` (Fire27 physical buttons / CoreS3 FT6336U
touch zones) is mapped to LVGL navigation keys feeding an oxivgl `KeypadState`,
which `run_app_nav_keypad_events` reads and routes to the view's focus group.
Per-board bring-up is `board::lvgl_bringup` (display via
`board::spi2::into_display_only` + the right `Input`); the flush glue, view, and
the input adapters live in `examples/demos/src/ui/`, so `src/bin/lvgl.rs` is
only the wiring. **CoreS3 additionally drives a direct-touch POINTER indev**
(oxivgl 0.5 `PointerIndev`, fed by an async FT6336U poll task that bridges the
I2C read into the indev's sync read callback), so on-screen widgets can be
tapped by coordinate — the bottom-strip keys stay on the button API for
focus-nav (multi-tap / long-press, `multi_tap_ms` tuned to 150 ms) *and* taps
land anywhere on screen (#32 I3).

Notes:

- The flush uses an explicit `SpiDmaBus` (`.with_dma`/`.with_buffers`): Fire27
  drives SPI2 on GPIO18/23 over PDMA (a *plain* `Spi::into_async()` flush goes
  "usr-stuck" after the first frame; a descriptor-backed DMA bus avoids it);
  CoreS3 drives SPI2 on GPIO36/37 over GDMA, with the panel reset via the AW9523
  expander and the backlight via the AXP2101 (no GPIO reset/backlight pins).
- **Logging is the BSP console on both boards** (`io::console`, no probe). Read
  it over the serial port — Fire27 at **1 Mbaud** (e.g. `espflash monitor
  --baud 1000000`, or `screen /dev/tty… 1000000`), CoreS3 at the default rate on
  the USB-Serial-JTAG CDC port. Flash CoreS3 with `probe-rs download` + `reset`
  (or `espflash`), then read the **CDC port**, not RTT. **Never emit a per-frame
  log stream** over the console — an undrained UART/CDC back-pressures the ring
  and can stall the render loop (HIL-confirmed at oxivgl Trace level); keep the
  LVGL demo at `Info`. A panic is reported once at the *next* boot as the
  `[bc] previous panic` breadcrumb line, so a crash is never silently lost.
- `oxivgl-sys` downloads and compiles LVGL 9.5 at build time, so this example
  needs network access, the target C compiler (`xtensa-esp32{,s3}-elf-gcc`) and
  `libclang` for `bindgen` (with `BINDGEN_EXTRA_CLANG_ARGS` pointing at the
  newlib sysroot) — all provided by the devcontainer.

## Dependencies & the esp-hal fork

The **library** depends only on **stock crates.io** crates (`esp-hal` 1.1.1,
`esp-radio` 0.18.0, `esp-sync`, `esp-alloc`) — it uses no fork-specific API
(the 1-Wire-over-RMT driver is vendored in-tree; see `driver::onewire`).

The **examples**, and all local workspace builds, are redirected to a fork —
[`emobotics-dev/esp-hal`](https://github.com/emobotics-dev/esp-hal/tree/local) —
via `[patch.crates-io]`. The fork is esp-hal 1.1.1 plus a small set of **ESP32
fixes not yet upstream**, primarily **SPI-DMA correctness** that the LVGL
display example's `SpiDmaBus` flush depends on:

- `feat(spi)` — zero-copy DMA in `write_async`
- `fix(spi-dma)` — ESP32 PDMA TX unaligned-length wedge (chained-descriptor fix)
- `fix(spi/dma)` — recover from an RX descriptor fault instead of hanging
- `fix(spi)` — bound the ESP32 post-DONE busy-re-wake (silent SD-card wedge)

plus assorted ESP32 robustness fixes (linker stack-guard sizing, I2C NACK
handling, `esp-println` UART critical-section bound).

Both the `[patch]` and the example git dependencies are pinned to a **commit
rev**, not a branch, so builds are reproducible. `cargo publish` ignores
`[patch]`, so a published `m5stack-core` resolves the plain crates.io versions.
The example UI crate uses the **crates.io** `oxivgl` 0.5 / `oxivgl-sys` 0.2
releases (no git pin).

**Roadmap:** upstream these patches; once they land in a released esp-hal, the
fork and the `[patch]` are dropped.

## Design

- **Chip differences** handled via `#[cfg(feature = "...")]` (e.g. RMT channel in `ds18b20`)
- **`SharedI2cBus`** wraps `Mutex<RawMutex, I2c>` — safe for single-executor async tasks
- **Resource pattern**: `*Resources` structs bundle peripherals, consumed by `into_driver()` or task loops
- **IO loops** use error counting with threshold (e.g. PPS breaks after 10 consecutive errors)
- **GPIO35 (CoreS3)**: GPIO35 is the display DC line (and is hardware-shared with SPI2 MISO). The cores3 example uses no SD/MISO, so it drives DC as a plain `Output` — `Output::new` configures the pad's IO-MUX so the pin actually drives. (A consumer that *also* needs MISO on the same bus, like alternator-regulator's SD card, must instead claim GPIO35 as MISO and toggle DC via register-level muxing.)

## License

Licensed under either of **MIT** ([LICENSE-MIT](LICENSE-MIT)) or **Apache-2.0**
([LICENSE-APACHE](LICENSE-APACHE)) at your option.
