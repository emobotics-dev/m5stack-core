# m5stack-core

Board support crate for **M5Stack Fire27** (ESP32) and **CoreS3** (ESP32-S3).

Provides chip-agnostic drivers, shared I2C bus, and reusable async IO task loops with `fn(...)` callbacks.

## Features

| Feature | Target | Chip |
|---------|--------|------|
| `fire27` | `xtensa-esp32-none-elf` | ESP32 |
| `cores3` | `xtensa-esp32s3-none-elf` | ESP32-S3 |

Exactly one feature must be enabled.

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

### Memory (`mem::`)

PSRAM heap integration, behind the **`psram`** Cargo feature. Both boards have
external SPI PSRAM (Fire27 ~4 MB, CoreS3 ~8 MB). `mem::init_psram_heap(peripherals.PSRAM)`
maps it and registers it as an external region of the `esp-alloc` global heap,
returning the free PSRAM bytes. Applications can then allocate from it either
implicitly (the global allocator spills into PSRAM after internal DRAM) or
**explicitly** — preferably via the *checked* helpers:

```rust
use m5stack_core::mem;

let psram_free = mem::init_psram_heap(peripherals.PSRAM);
let mut big = mem::psram_vec::<u8>(512 * 1024);  // in PSRAM; atomics rejected at compile time
let scratch = mem::psram_box([0u32; 1024]);      // in PSRAM
let dma = mem::dma_buffer(4 * 1024);             // in internal DRAM; DMA-safe
```

The raw markers `ExternalMemory` / `InternalMemory` are still re-exported for
direct `allocator_api2` use, but they skip the atomic check — use them only when
you know what's going into PSRAM.

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

### Serial console (`io::console`)

The **complete** async logging console for the firmware — both the target-agnostic
pipeline AND the per-target hardware. No `esp-println`/`esp-backtrace`.

- `init()` / `enable_async()` — register the `log::Log` backend (boots blocking;
  switches to the async drain once spawned).
- `setup(...) -> (ConsoleRx, ConsoleTx)` — build + split the peripheral
  (fire27: UART0 @ 1 Mbaud; cores3: USB-Serial-JTAG) into the RX half (→
  `serial_cmd`) and the TX half (→ the drain task). The binary owns `into_async()`
  so the IRQ binds to the calling core.
- `drain_task(ConsoleTxAsync)` — the single console writer (`#[embassy_executor::task]`);
  drains the cross-core queue to the async TX sink.
- `send_line(Arguments)` — back-pressuring emit for bulk dumps (the `:cat`
  read-back); awaits queue space instead of dropping.
- `boot_panic_write(&[u8])` (internal) — boot/panic raw-FIFO poke, bounded (drops on
  a full/host-less FIFO so it never wedges the radio). Bounded-spin on TX-FIFO
  status — an anti-pattern reserved for the two contexts where the async drain
  cannot run; do NOT call from steady-state code.
- `on_panic(&PanicInfo) -> !` — shared message-only panic print + halt, used by
  both binaries' `#[panic_handler]`.

`alternator-regulator` depends on this crate (optional, esp-hal-gated) only so
`logger::cat_line` can call `send_line`; host builds never pull it.

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

Each board crate is a set of small, single-topic binaries (one subsystem each)
rather than one kitchen-sink demo, so each is copy-pasteable as a starting point.
Chip-agnostic helpers (colour wheel, splash/status rendering, I2C scan) live in
the shared `examples/common` crate; per-board chip bring-up lives in each crate's
`src/lib.rs`. Select a binary with `--bin <name>`.

`WIFI_SSID`/`WIFI_PASSWORD` are read at build time; unset → WiFi is skipped and
the display still runs. The `coex` bins need `--features coex` and must be built
`--release` (the BLE deps trip a dev-profile xtensa codegen bug). The `m5go` bin
needs the M5GO Battery Bottom attached to do anything visible.

### Fire27 (ESP32)

```bash
cargo +esp run --release -p fire27 --bin <name>
```

| bin        | what it shows                                   | needs |
|------------|-------------------------------------------------|-------|
| `display`  | splash + 3-button (39/38/37) readout, no radio  | — |
| `i2c_scan` | I2C bus scan (0x08..0x77), addresses on LCD     | — |
| `m5go`     | SK6812 LEDs (G15) colour-wheel + IP5306 battery %| M5GO bottom attached |
| `wifi_sta` | WiFi STA + DHCP + AP scan, IP on LCD            | `WIFI_SSID`/`WIFI_PASSWORD` |
| `coex`     | `wifi_sta` plus a BLE peer-MAC scanner          | `--features coex`, `--release` |

```bash
WIFI_SSID=myssid WIFI_PASSWORD=secret cargo +esp run --release -p fire27 --bin wifi_sta
WIFI_SSID=myssid WIFI_PASSWORD=secret cargo +esp run --release -p fire27 --bin coex --features coex
```

GPIO: I2C SDA=21/SCL=22, SPI CLK=18/MOSI=23/MISO=19, Display CS=14/DC=27/RST=33/BL=32, Buttons=39/38/37, M5GO LEDs=15, IP5306@0x75.

### CoreS3 (ESP32-S3)

```bash
cargo +esp run --release -p cores3 --bin <name> --target xtensa-esp32s3-none-elf
```

| bin        | what it shows                                              | needs |
|------------|-----------------------------------------------------------|-------|
| `display`  | splash + capacitive-touch readout, no radio               | — |
| `i2c_scan` | I2C bus scan (0x08..0x77), addresses on LCD               | — |
| `m5go`     | SK6812 LEDs (G13) colour-wheel + AXP2101 battery (mV) + M-Bus 5V enable | M5GO bottom attached |
| `wifi_sta` | WiFi STA + DHCP + AP scan, IP on LCD                      | `WIFI_SSID`/`WIFI_PASSWORD` |
| `coex`     | `wifi_sta` plus a BLE peer-MAC scanner                    | `--features coex`, `--release` |

The `m5go` bin enables the M-Bus 5V rail (off by default on CoreS3) via the
AW9523 expander, guarded against shared-VBUS contention — see the M5GO Battery
Bottom section above.

```bash
WIFI_SSID=myssid WIFI_PASSWORD=secret \
  cargo +esp run --release -p cores3 --bin wifi_sta --target xtensa-esp32s3-none-elf
WIFI_SSID=myssid WIFI_PASSWORD=secret \
  cargo +esp run --release -p cores3 --bin coex --features coex --target xtensa-esp32s3-none-elf
```

GPIO: I2C SDA=12/SCL=11, SPI CLK=36/MOSI=37, Display CS=3/DC=35, RST via AW9523B, BL via AXP2101 DLDO1, M5GO LEDs=13, AXP2101@0x34.

### LVGL UI (`examples/lvgl`, Fire27 + CoreS3)

A separate example crate that drives the panel with
**[oxivgl](https://github.com/emobotics-dev/oxivgl)** (safe LVGL 9 bindings)
instead of `embedded-graphics` — an LVGL render loop with the SPI flush running
on a high-priority `InterruptExecutor`, so the UI animates smoothly while the
main task keeps working. The demo shows a title, an animated spinner and a
frame counter. Builds for both boards (default `fire27`):

```bash
cargo +esp run --release -p lvgl-example --bin lvgl                                              # Fire27
cargo +esp run --release -p lvgl-example --bin lvgl --no-default-features --features cores3 \
  --target xtensa-esp32s3-none-elf                                                               # CoreS3
```

Notes:

- The flush uses an explicit `SpiDmaBus` (`.with_dma`/`.with_buffers`): Fire27
  drives SPI2 on GPIO18/23 over PDMA (a *plain* `Spi::into_async()` flush goes
  "usr-stuck" after the first frame; a descriptor-backed DMA bus avoids it);
  CoreS3 drives SPI2 on GPIO36/37 over GDMA, with the panel reset via the AW9523
  expander and the backlight via the AXP2101 (no GPIO reset/backlight pins).
- **Logging differs by board.** Fire27 logs over UART (`esp-println`/
  `esp-backtrace`). CoreS3 uses **RTT** (`rtt-target` + `panic-halt`) read via
  `probe-rs run`/`attach`, since `esp-println`/`esp-backtrace` conflict with the
  USB-Serial-JTAG. **The RTT logger runs at `Info`, not Trace** — and this
  matters: at Trace, oxivgl's per-frame DEBUG stream floods the RTT buffer, and
  with no debugger draining it the channel back-pressures and **stalls the
  render loop** (HIL-confirmed freeze). At Info the demo emits only a few startup
  lines and runs standalone. (More generally: never emit a per-frame log stream
  over an undrained RTT/USB-Serial-JTAG/UART channel — it will eventually block.)
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
The example UI crate's `oxivgl`/`oxivgl-sys` deps are likewise rev-pinned.

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
