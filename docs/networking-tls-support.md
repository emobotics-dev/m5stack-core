# Networking + TLS support (WiFi, PSRAM, peripheral split)

Requirements for the BSP to support a networked application that runs TLS via
`esp-rs/mbedtls-rs` (driving consumer: **oxicharge**, an ISO 15118-20 stack). The
crypto itself is **not** a BSP concern — `mbedtls-rs` claims the generic ESP32-S3
crypto peripherals directly. The BSP's job is board-common bringup (WiFi, PSRAM) and
to **leave the crypto peripherals for the application**.

## What the BSP should provide

### WiFi → return an `embassy-net` `Stack`
Board-common and reusable across projects, so it lives here (not in each binary).

- Take `peripherals.WIFI` + a radio timer (`TIMG`) + the started `esp-rtos`.
- Init `esp-radio` (`wifi` feature) and build the `embassy_net::Stack`.
- **Take a `seed: u64` parameter** rather than grabbing `RNG` — the application owns
  the `Trng` (for TLS) and derives the stack seed from it. Keeps `RNG`/`ADC1` free
  for the app (see below).
- Spawn the connection + net runner tasks; expose the `Stack` (and a "link up" wait).

### PSRAM → init + register the heap
Board-specific (CoreS3 in-package PSRAM) and reusable. mbedtls is heap-hungry
(~100+ KB of handshake/cert buffers per session), which does **not** fit comfortably
in internal SRAM alongside the WiFi stack — so the esp-alloc heap must be PSRAM-backed.

- `esp_hal::psram::Psram::new(peripherals.PSRAM, PsramConfig { .. })` with the board's
  config, then register the mapped range with `esp-alloc`.

## What the BSP must NOT take (left for the application)

`mbedtls-rs` (via its `EspAccel` / `Trng` / RTC hooks) claims these generic peripherals
in the **binary**, not the BSP. The BSP's init must take only what it needs (display
SPI, touch I²C, PSRAM, WIFI, a radio TIMG) **by field**, leaving these available:

| Peripheral | Used by the app for | mbedtls tier |
|---|---|---|
| `RNG` + `ADC1` | `TrngSource`→`Trng`→`Tls::new(trng)` — entropy (ADC1 is the true-RNG source) | **mandatory (security)** |
| `LPWR` | `Rtc` → wall clock for X.509 validity (`hook_wall_clock`) | mandatory |
| `SHA` + `RSA` | `EspAccel::new(SHA, RSA)` — HW-accel SHA-1/256/512 + modexp (RSA/MPI) | performance (SW fallback ok) |

Notes for the consumer (not BSP work): AES-GCM record crypto and the dedicated ECC
accelerator are **not** wired by the current `mbedtls-rs` (P-256 uses the MPI-accelerated
bignum path). Functional, just not HW-accelerated.

## Peripheral-ownership pattern

```
binary: let p = esp_hal::init(cfg);          // owns all peripherals
        esp_rtos::start(timg0.timer0, ..);
        let board = m5stack_core::init(BoardPeripherals {
            // display SPI, touch I2C, PSRAM, WIFI, radio TIMG, ...
        }, seed);                              // BSP: display + PSRAM heap + WiFi Stack
        // app keeps p.SHA, p.RSA, p.RNG, p.ADC1, p.LPWR  → mbedtls-rs
```

## BSP-session checklist
- [ ] PSRAM init + esp-alloc heap region (CoreS3 config).
- [ ] WiFi init returning `embassy_net::Stack`, taking `seed: u64` (not `RNG`).
- [ ] BSP `init` signature takes peripherals **by field**; does **not** consume
      `SHA`/`RSA`/`RNG`/`ADC1`/`LPWR`.
- [ ] Confirm HW-accel path: `mbedtls-rs/esp32s3` feature + `EspAccel(SHA, RSA)` builds
      and the SHA/MPI hooks engage on-target.
