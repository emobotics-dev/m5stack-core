#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Build the HIL harness if it is out of date, then run it. Every argument is
# forwarded verbatim.
#
# That is the whole job, and it is why this wrapper is stable: it encodes where
# the harness lives and nothing about what it does, so a new harness flag never
# needs a change here. Anything that would want editing per run belongs on the
# command line, not in this file.
#
#   tools/hil.sh --board <MAC> --read-identity
#   tools/hil.sh --board <MAC> --ensure-image target/…/display --capture 15
#
# `cargo run` rather than a hardcoded path to the binary: the harness pins its
# own host target in `m5stack-core-hil/.cargo/config.toml`, and letting cargo
# resolve the output path means this script does not restate the triple.
#
# `cd` into the crate, NOT `--manifest-path`: cargo reads .cargo/config.toml
# from the CURRENT DIRECTORY, not from the manifest's. Invoked from the repo
# root, `--manifest-path` would apply the ROOT config — which pins an Xtensa
# target — and the build dies with "can't find crate for `std`".
#
# BUILD in the crate directory, then RUN from the caller's. `cargo run` would
# have to stay `cd`'d, and the harness would then resolve every relative path
# the caller gave — `--ensure-image target/…/display` — against the wrong
# directory. Splitting the two keeps cargo's config correct AND the caller's
# paths meaningful.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
hil="$root/m5stack-core-hil"

( cd "$hil" && cargo build --quiet --release --bin m5stack-hil >&2 )

# The output triple comes from the crate's own .cargo/config.toml, so it is
# discovered rather than restated here — one less thing to keep in sync.
bin="$(ls -t "$hil"/target/*/release/m5stack-hil 2>/dev/null | head -1)"
if [ -z "$bin" ]; then
    echo "tools/hil.sh: built m5stack-hil but could not find the binary under $hil/target" >&2
    exit 1
fi

# Default the transcript directory to the repo's gitignored work/ unless the
# caller chose one, so captures never land somewhere that gets committed.
out=()
case " $* " in
    *" --out "*) ;;
    *) out=(--out "$root/work/hil") ;;
esac

exec "$bin" "${out[@]}" "$@"
