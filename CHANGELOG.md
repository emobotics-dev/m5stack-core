# Changelog

All notable changes to this crate are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
