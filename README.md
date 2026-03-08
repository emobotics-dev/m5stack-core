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

GPIO: I2C SDA=12/SCL=11, SPI CLK=36/MOSI=37, Display CS=3/DC=35(muxed), RST via AW9523B, BL via AXP2101 DLDO1.

## Design

- **Chip differences** handled via `#[cfg(feature = "...")]` (e.g. RMT channel in `ds16b20`)
- **`SharedI2cBus`** wraps `Mutex<RawMutex, I2c>` — safe for single-executor async tasks
- **Resource pattern**: `*Resources` structs bundle peripherals, consumed by `into_driver()` or task loops
- **IO loops** use error counting with threshold (e.g. PPS breaks after 10 consecutive errors)
- **GPIO35 muxing** (CoreS3): `Gpio35Dc` implements `OutputPin` via direct register writes — GPIO35 is shared between SPI MISO and display DC

## License

BSD-3-Clause
