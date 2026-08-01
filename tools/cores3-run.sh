#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Build one demo binary for the CoreS3, make sure the board is running exactly
# that image, and capture what it says.
#
#   tools/cores3-run.sh display                      # 15 s capture
#   tools/cores3-run.sh display 30
#   tools/cores3-run.sh display 30 --until 'wifi: got ip'
#
# This replaces the flash-then-monitor dance that used to be retyped per run:
# no `stty`, no background `cat` racing a reset, no `sleep` before it, and no
# guessing whether the board is running the image just built. `--ensure-image`
# resets, reads the board's identity, and writes only on a mismatch — a match
# costs ~1.5 s against ~40 s for a flash.
#
# Stability: the board is NAMED, never spelled out. `cores3` is looked up in
# hil.toml, so a new MAC or a second rig is a config edit and not a change here.
# Everything after the seconds argument is forwarded to the harness verbatim, so
# a new harness flag needs no change either.
#
#   HIL_BOARD        board name in hil.toml            (default: cores3)
#   HIL_RIG          rig name in hil.toml              (default: its default_rig)
#   CORES3_FEATURES  cargo features for the demo build (default: cores3)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

BIN="${1:-}"
if [ -z "$BIN" ]; then
    echo "usage: $0 <bin> [seconds] [extra m5stack-hil flags...]" >&2
    exit 2
fi
shift

SECS=15
if [ $# -gt 0 ] && [[ "$1" =~ ^[0-9]+$ ]]; then
    SECS="$1"
    shift
fi

BOARD="${HIL_BOARD:-cores3}"
FEATURES="${CORES3_FEATURES:-cores3}"
TARGET=xtensa-esp32s3-none-elf

# Build chatter goes to stderr, so `$(tools/cores3-run.sh …)` still captures
# only what the harness itself puts on stdout.
cargo build --release --manifest-path "$root/Cargo.toml" \
    -p demos --no-default-features --features "$FEATURES" \
    --target "$TARGET" --bin "$BIN" >&2

exec "$root/tools/hil.sh" \
    --board "$BOARD" ${HIL_RIG:+--rig "$HIL_RIG"} \
    --ensure-image "$root/target/$TARGET/release/$BIN" \
    --capture "$SECS" "$@"
