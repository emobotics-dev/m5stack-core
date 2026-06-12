# Changelog

All notable changes to this crate are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
