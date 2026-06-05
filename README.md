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
| `ds16b20` | 1-Wire temperature sensor via RMT (chip-specific RMT channel selection) |
| `aw9523b` | I2C GPIO expander (CoreS3, 0x58) — LCD/touch reset pulses |
| `axp2101` | PMIC (CoreS3, 0x34) — backlight voltage, battery ADC, VBUS detection |
| `ft6336u` | Capacitive touch controller (0x38) — stateless `read_touch()` |
| `radio` | BLE radio init wrapper (`BleConnector` from `esp-radio`) |

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

### Fire27 (ESP32)

Display demo with I2C scan and button polling.

```bash
cargo +esp run --release -p fire27
```

GPIO: I2C SDA=21/SCL=22, SPI CLK=18/MOSI=23/MISO=19, Display CS=14/DC=27/RST=33/BL=32, Buttons=39/38/37.

### CoreS3 (ESP32-S3)

Display demo with AW9523B/AXP2101 init, I2C scan, and touch polling.

```bash
cargo +esp run --release -p cores3 --target xtensa-esp32s3-none-elf
```

GPIO: I2C SDA=12/SCL=11, SPI CLK=36/MOSI=37, Display CS=3/DC=35, RST via AW9523B, BL via AXP2101 DLDO1.

## Design

- **Chip differences** handled via `#[cfg(feature = "...")]` (e.g. RMT channel in `ds16b20`)
- **`SharedI2cBus`** wraps `Mutex<RawMutex, I2c>` — safe for single-executor async tasks
- **Resource pattern**: `*Resources` structs bundle peripherals, consumed by `into_driver()` or task loops
- **IO loops** use error counting with threshold (e.g. PPS breaks after 10 consecutive errors)
- **GPIO35 (CoreS3)**: GPIO35 is the display DC line (and is hardware-shared with SPI2 MISO). The cores3 example uses no SD/MISO, so it drives DC as a plain `Output` — `Output::new` configures the pad's IO-MUX so the pin actually drives. (A consumer that *also* needs MISO on the same bus, like alternator-regulator's SD card, must instead claim GPIO35 as MISO and toggle DC via register-level muxing.)

## License

BSD-3-Clause
