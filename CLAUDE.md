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

## Dependency policy

**Latest-and-greatest, except where the esp-hal 1.1.1 stack forbids it.** Track
the newest version of everything; the only legitimate reason to hold a dep back
is a constraint imposed by esp-hal/esp-radio/esp-alloc/esp-sync (or by another
dep that is itself already at its latest).

A hold must say *why*, inline, and the reason must be **verified rather than
assumed** — attempt the bump and record what actually breaks. Re-verified
2026-08-01 by doing exactly that:

| held | latest | what happens on bump |
|---|---|---|
| `allocator-api2` 0.3 | 0.4 | `E0277` at `src/mem.rs:351` — esp-alloc 0.10 implements **0.3**'s `Allocator` for `Internal`/`ExternalMemory`; 0.4 is a different trait ("expected"/"found" both named `Allocator`) |
| `fixed` 1.29 | 1.31 | resolution fails — 1.30+ needs `az ^1.3`, embedded-graphics 0.8.2 needs `az ~1.2.0` (a **normal** dep, and e-g is already latest) |
| `trouble-host` 0.6 | 0.7 | cargo *does* resolve it, compiling bt-hci 0.8.1 **and** 0.9.0 side by side. Port `ble.rs` first (0.7 moved the controller into `HostResources`' first generic, and `build()` yields a `Stack`) — the pre-port errors are misleading. The real blocker then shows plainly: `BleConnector: bt_hci::transport::Transport` unsatisfied, with *"expected `FromHciBytesError`, found `FromHciBytesError`"* — the two-versions signature. Gated on **esp-radio**, not esp-hal: 0.18.0 wants `esp-hal ~1.1.0-rc.0` (1.1.1 satisfies it), and **no published esp-radio uses bt-hci 0.9**, not even `1.0.0-beta.0` |

`cargo update --workspace` locking 0 packages is the quick check that nothing
else has drifted. MSRV tracks the esp-hal family's (**1.88**), not something
lower — claiming lower is a promise the crate cannot keep.

## HIL (hardware-in-the-loop)

**Use the harness — do not hand-roll a runner.** `m5stack-core-hil` (host crate,
binary `m5stack-hil`) owns claiming a board, flashing it only when it is not
already running the image, and capturing output without losing any. Wrappers and
setup: [`tools/README.md`](tools/README.md). One-time: `cp hil.toml.example
hil.toml`.

```sh
tools/cores3-run.sh display 20            # build, ensure the image, capture
tools/hil.sh --board cores3 --read-identity
```

Boards are **named** in `hil.toml`, never spelled out at a call site: a MAC (and
the `/dev/serial/by-id/...` path derived from it) is a fact about a rig, so
swapping hardware or adding a second rig is a config edit and nothing else.

- **CoreS3**: covered by the harness — reset over **JTAG** (`probe-rs`, with the
  probe named explicitly so a multi-board rig cannot reset the wrong one), flash
  via `espflash`, console on its USB-Serial-JTAG. The JTAG reset is what lets
  the harness attach *before* resetting and so catch the identity line at
  ~0.3 s; an RTS reset cannot, because it needs the port itself.
- **Fire27**: not yet wired into the harness (next PR). Meanwhile: `espflash
  flash --monitor --monitor-baud 1000000 <elf>`; **console is UART0 @ 1 Mbaud**,
  not 115200.
- Serial devices are per-board; use `/dev/serial/by-id/*`. Some bench serials are
  off-limits — check before flashing.
- Display verification: capture the panel with the phone camera (the
  `phone-camera` skill / ADB). Release the ADB claim when done.
- Anything ad-hoc you write for one investigation goes in gitignored `work/`. If
  you find yourself editing a runner script to do a run, that script is wrong —
  the varying part belongs on the command line or in `hil.toml`.

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

## Docs hosting (docs.rs is red on purpose)

**docs.rs has never built this crate and never will** — don't try to fix it.
Its stock rust-lang toolchain has no Xtensa backend, and both boards are Xtensa
(ESP32 / ESP32-S3), so no feature or `[package.metadata.docs.rs]` setting helps.
(esp-hal dodges this by pointing `default-target` at one of its RISC-V chips —
this crate has no RISC-V board to fall back to. `esp-idf-hal` hits the same wall
and self-hosts too; that's the precedent. See #36.)

- The rustdoc is built by `.forgejo/workflows/docs.yml` (both boards, `esp`
  toolchain) on every push to `master` and force-pushed to the `gh-pages` branch
  of the **GitHub mirror**, served at
  `https://emobotics-dev.github.io/m5stack-core` — the crate's `documentation`
  link.
- That push needs `GH_PAGES_TOKEN` (a fine-grained GitHub PAT on the mirror,
  `Contents: read+write`) as a Forgejo Actions secret, and Pages set to the
  `gh-pages` branch root. Both are one-time human steps; without the secret the
  job builds the docs and skips the push. How to mint the token and where to
  paste it — deep links included — is the numbered block above the publish step
  in `.forgejo/workflows/docs.yml`; it is the only copy, don't restate it here.

## Layout

- `src/board/` — per-board pin wiring + bring-up (`fire27`, `cores3`, `spi2`,
  `display`). `src/io/` — console, buttons, watchdog, sensor loops. `src/mem/` —
  global heap + PSRAM. `src/driver/` — onewire, radio, etc.
- `examples/demos/` — one crate, one bin per subsystem, board via cargo feature.
- `m5stack-core-build/` — **host** build-script helper for the `identity`
  feature (`emit_identity_env`). Its own `.cargo/config.toml` pins the host
  target, without which `cargo test` cross-compiles it to the board and cannot
  build; run its tests from inside the crate directory, never `-p` from the
  root.
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
