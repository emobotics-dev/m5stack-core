#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Build one demo binary for a board, make sure that board is running exactly
# that image, and capture what it says.
#
#   tools/demo-run.sh cores3 display                      # 15 s capture
#   tools/demo-run.sh fire27 onewire 30
#   tools/demo-run.sh fire27 m5go 30 --until 'button: A'
#
# Usually reached through `tools/cores3-run.sh` / `tools/fire27-run.sh`, which
# are this script with the board kind filled in.
#
# This replaces the flash-then-monitor dance that used to be retyped per run:
# no `stty`, no background `cat` racing a reset, no `sleep` before it, and no
# guessing whether the board is running the image just built. `--ensure-image`
# resets, reads the board's identity, and writes only on a mismatch.
#
# Stability: the board is NAMED, never spelled out. `cores3`/`fire27` are looked
# up in hil.toml, so a new MAC, a new port or a second rig is a config edit and
# not a change here. The only thing this file knows that the config does not is
# how to BUILD for each board — a target triple and a cargo feature — which is a
# fact about the repo rather than about a rig. Everything after the seconds
# argument is forwarded to the harness verbatim, so a new harness flag needs no
# change either.
#
#   HIL_BOARD      board name in hil.toml            (default: the board kind)
#   HIL_RIG        rig name in hil.toml              (default: its default_rig)
#   DEMO_FEATURES  cargo features for the example build (default: ex-<kind>)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

KIND="${1:-}"
BIN="${2:-}"
if [ -z "$KIND" ] || [ -z "$BIN" ]; then
    echo "usage: $0 <cores3|fire27> <example> [seconds] [extra m5stack-hil flags...]" >&2
    exit 2
fi
shift 2

# How to build for each board. The one board-specific thing here, kept as a
# table so adding a third is a line rather than another copy of this file.
case "$KIND" in
    cores3) TARGET=xtensa-esp32s3-none-elf ;;
    fire27) TARGET=xtensa-esp32-none-elf ;;
    *)
        echo "$0: unknown board kind '$KIND' (expected cores3 or fire27)" >&2
        exit 2
        ;;
esac

SECS=15
if [ $# -gt 0 ] && [[ "$1" =~ ^[0-9]+$ ]]; then
    SECS="$1"
    shift
fi

BOARD="${HIL_BOARD:-$KIND}"
# `ex-<kind>` is the board aggregate the examples need — see the `ex-*` block in
# the root Cargo.toml. `required-features` can only gate an example, never turn
# anything on, so the feature set is always named here.
FEATURES="${DEMO_FEATURES:-ex-$KIND}"

# `--no-default-features` for both boards: the library has no defaults, and a
# build whose feature set depends on a default is a build that changes meaning
# when that default moves.
#
# Build chatter goes to stderr, so `$(tools/demo-run.sh …)` still captures only
# what the harness itself puts on stdout.
cargo build --release --manifest-path "$root/Cargo.toml" \
    --no-default-features --features "$FEATURES" \
    --target "$TARGET" --example "$BIN" >&2

exec "$root/tools/hil.sh" \
    --board "$BOARD" ${HIL_RIG:+--rig "$HIL_RIG"} \
    --ensure-image "$root/target/$TARGET/release/examples/$BIN" \
    --capture "$SECS" "$@"
