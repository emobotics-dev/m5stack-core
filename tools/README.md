<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# tools/

Committed, curated helpers. Not a junk drawer: a script lives here because it is
meant to be run again unchanged. Anything pinned to one day's state belongs in
the gitignored `work/`, or nowhere.

The rule these scripts are built to satisfy: **a helper that needs editing per
run is not a helper.** Everything that varies — which board, which rig, which
binary, how long — is an argument or a config entry, never a line to retype.

## Setup, once

```sh
cp hil.toml.example hil.toml     # then edit for your bench
```

`hil.toml` is gitignored because it describes *a rig*, not the project. It maps
board **names** to MACs and ports, so nothing else ever spells one out — a
swapped board or a second rig is an edit to that file alone. Unknown keys in it
are an error, not something silently skipped.

## `hil.sh` — run the harness

Builds `m5stack-core-hil` if needed and forwards every argument to it.

```sh
tools/hil.sh --board cores3 --read-identity
tools/hil.sh --board cores3 --ensure-image target/xtensa-esp32s3-none-elf/release/display
tools/hil.sh --board cores3 --capture 20 --until 'wifi: got ip'
tools/hil.sh --help
```

It encodes *where the harness lives* and nothing about what it does, which is
why a new harness flag never needs a change here.

## `cores3-run.sh` / `fire27-run.sh` — build, ensure, capture

The everyday ones. Build a demo binary for a board, guarantee the board is
running that exact image, and capture its output.

```sh
tools/cores3-run.sh display              # build, ensure, capture 15 s
tools/cores3-run.sh display 30
tools/cores3-run.sh display 30 --until 'wifi: got ip'

tools/fire27-run.sh m5go
tools/fire27-run.sh onewire 30           # onewire is Fire27-only
```

| variable | default | meaning |
|---|---|---|
| `HIL_BOARD` | the board kind | board name in `hil.toml` |
| `HIL_RIG` | the file's `default_rig` | rig name in `hil.toml` |
| `DEMO_FEATURES` | the board kind | cargo features for the demo build |

Trailing arguments are forwarded to `m5stack-hil` verbatim.

Both are one line each: they name a board kind and hand everything else to
`demo-run.sh`, which is where the build knowledge lives (a target triple and a
cargo feature per board, as a table). A third board is an entry in that table
rather than a third copy of the script — and the two existing ones cannot drift
apart, because there is only one of them.

Everything else a board needs — which port, which baud, how to reset it, what to
flash it with — is in `hil.toml` and not here, so the scripts stay identical
across rigs.

## Why this replaces the hand-written runners

Everything below used to be retyped, or rewritten, per investigation:

- **flash-then-monitor.** `--ensure-image` resets the board, reads the identity
  it prints at boot, and writes the image *only* if it differs — then verifies
  the write took. Measured on this bench: ~6 s for a match against ~15 s for a
  write, and a write
  that silently did not take is caught instead of measured.
- **`stty` + a background `cat`.** The port is owned by a value and closed by
  `Drop` on every path including panic. An orphaned reader holding a tty — so
  the next run reports "Device or resource busy" and a live board looks dead —
  cannot happen.
- **`sleep 2` before a reset.** Waits are bounded and named: they return the
  instant the condition holds and, on expiry, say *which* condition failed and
  why.
- **Racing a reader against the reset.** The CoreS3's USB-Serial-JTAG discards
  output when nobody is reading, and the identity line goes out at ~0.3 s of
  uptime — so a runner that resets *then* attaches misses it every time
  (measured: 0 bytes captured from a board that was demonstrably printing). The
  harness never opens that window: it attaches **first** and holds the same fd
  right through the reset, by whichever route the board allows.
  - **CoreS3** — reset over **JTAG** (`probe-rs`), which does not re-enumerate
    the USB device, so the descriptor stays valid across it.
  - **Fire27** — no probe, so the harness pulses the tty's **RTS** line itself,
    on the port it is already holding. The USB-serial bridge is a separate chip
    from the ESP32 and does not reset with it, so again nothing is released.
  - `reset = "espflash"` remains available and is the only route that *does*
    inherit the race: a subprocess needs the port to itself, so the harness has
    to let go across the reset. It is a fallback, not a default.
- **truncate-then-retry.** The capture is append-only and streamed to disk as
  bytes arrive, so a failing attempt's evidence survives the recovery from it —
  and a run that dies still has its transcript.
- **`readline()`-style capture.** Raw bytes are kept, and lines are a derived
  view. A board that dies mid-sentence never sends its closing newline, and
  those bytes are exactly the ones worth having.

Two runs cannot fight over a board either: the harness takes a lockfile keyed to
the board's stable id and, if another process holds it, **refuses and names the
holder** rather than killing it.

## Flash frequency, deliberately

Both boards default to `flash_freq = "80mhz"`. espflash's own default is 40 MHz;
code runs from flash by XIP, so an image written at 40 MHz measures slower than
the same image written at 80. Pinning it means a measurement never depends on
which tool wrote the image.

It is also the first thing to change if a freshly written **Fire27** does not
come up: `flash_freq = "40mhz"` in `hil.toml` is the safe value for a part that
cannot run at 80. The failure is loud rather than quiet — `--ensure-image`
verifies a write took by reading the board back, so a board that does not boot
is reported instead of measured.
