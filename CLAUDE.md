# CLAUDE.md — working notes for this repo

Board-support crate for M5Stack **Fire27** (ESP32) and **CoreS3** (ESP32-S3).
This file captures the non-obvious constraints; see `README.md` for the API tour.

## The publish gate (most important invariant)

`m5stack-core` publishes to crates.io. The **library** (`src/`, `[dependencies]`)
must therefore resolve **entirely from crates.io** — no `git =` / `path =` deps.

- The esp-hal family is pinned to **stock** versions in `[dependencies]`; a
  `[patch.crates-io]` redirects them to the emobotics esp-hal fork (a pinned rev)
  **only for local/example builds**. `cargo publish` ignores `[patch]`.
- The `sdspi` / `embedded-fatfs` / `block-device-adapters` fork is **git-only**
  (not on crates.io). It lives **only** in `examples/demos` behind the `sd`
  feature. **Never** let it into the library graph — that breaks publishing.
  Consequence: SD *bring-up* (bus, CS, 74-clock idle, GPIO35 mux) is the BSP's;
  the SD *driver* (`SdSpi`, FAT) stays in the consumer/example.
- Before releasing, verify: `cargo package --no-verify` then confirm the packaged
  `Cargo.toml` has no `git =` / `[patch]` (only the `[lib]` `path = "src/lib.rs"`).

## Building

- **Exactly one board feature is required** (`fire27` | `cores3`) — a featureless
  build hits a `compile_error!`. There are no default features.
- Default target is `xtensa-esp32-none-elf` (Fire27). For CoreS3 add
  `--target xtensa-esp32s3-none-elf`.
- `cores3` also needs `--no-default-features` when the demos crate defaults to
  `fire27`. Toolchain: the `esp` channel (`rust-toolchain.toml`).
- Example: `cargo build --release --features "cores3,display,psram" --target xtensa-esp32s3-none-elf`.

## HIL (hardware-in-the-loop)

- **CoreS3**: has a JTAG probe → `probe-rs download --chip esp32s3 <elf>` then
  `probe-rs reset --chip esp32s3`. Console on its USB-Serial-JTAG (115200).
- **Fire27**: no probe → `espflash flash --monitor --monitor-baud 1000000 <elf>`.
  **Console is UART0 @ 1 Mbaud**, not 115200.
- Serial devices are per-board; use `/dev/serial/by-id/*`. Some bench serials are
  off-limits — check before flashing.
- Display verification: capture the panel with the phone camera (the
  `phone-camera` skill / ADB). Release the ADB claim when done.

## No probabilistic fixes (hardware bugs)

Reproduce first, then root-cause. Don't lower a knob to dodge a race/hang — raise
it to reproduce reliably, then fix the cause. (See #50: the SD "mount hang" was
not a PDMA wedge but an `O(card-size)` `fs.stats()` FAT scan — proven by HIL, not
patched around.)

## Releasing

Pre-1.0 semver as Cargo interprets it: **breaking → bump minor (`0.x`), additive
/ fixes → bump patch (`0.x.y`)**. e.g. purely-additive APIs = a patch bump.

1. Bump `version` in `Cargo.toml`; add a `CHANGELOG.md` section (Keep a Changelog).
2. Build both boards with full features; run the publish-gate check above.
3. Commit the version bump (commit-message convention: see Layout below),
   tag `v<ver>`.
4. **Sync the GitHub mirror — required before publishing.** Development lives on
   Forgejo (LAN-only); crates.io's `repository` link points at the public GitHub
   mirror, so a published version whose commit/tag isn't on GitHub leaves that
   link dangling. Push the release commit + tag to `origin` (GitHub) so the
   mirror matches what you publish: `git push origin master --tags`. (GitHub
   master is force-synced from Forgejo — see Layout; a `master` ruleset may need
   lifting for a force-push if history was rewritten.)
5. `cargo publish --no-verify` — `--no-verify` because the crate needs an explicit
   board feature + xtensa target, so cargo's default host verify-build can't run
   (dual-board builds + `cargo package` are the manual substitute).
   Note: the `coex` feature has a known xtensa codegen issue; it is not a default
   and `--no-verify` skips the build, so it does not block publishing.

## Layout

- `src/board/` — per-board pin wiring + bring-up (`fire27`, `cores3`, `spi2`,
  `display`). `src/io/` — console, buttons, watchdog, sensor loops. `src/mem/` —
  global heap + PSRAM. `src/driver/` — onewire, radio, etc.
- `examples/demos/` — one crate, one bin per subsystem, board via cargo feature.
- Development is hosted on the self-hosted **Forgejo** instance
  (`http://forgejo:3000/emobotics/m5stack-core`, SSH `ssh://git@forgejo:222`);
  the public `github.com/emobotics-dev/m5stack-core` is the outward mirror
  (Forgejo is LAN-only) and stays the crate's `repository`/README/crates.io
  link. Commits are authored as *Holger Steinhaus* using the noreply email the
  history already uses (`3057137+hsteinhaus@users.noreply.github.com`);
  PR/issue/merge actions go through the `agent` bot account. Commit-message
  style, CLI usage and CI conventions all live in `emobotics/forgejo-instance`
  (`docs/agent-contributing.md`) — that doc is the source of truth and gets
  revised independently of this file, so don't restate its rules here; CI
  lives in `.forgejo/workflows/`.
