#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Build a demo for the Fire27, ensure the board runs it, capture its output.
#
#   tools/fire27-run.sh m5go                 # build, ensure, capture 15 s
#   tools/fire27-run.sh onewire 30
#   tools/fire27-run.sh wifi_sta 30 --until 'wifi: got ip'
#
# The board kind, and nothing else: everything this does lives in demo-run.sh,
# so the two boards cannot drift apart. Environment variables are documented
# there.
#
# The Fire27's console is UART0 at 1 Mbaud, and it has no debug probe — both are
# `hil.toml` entries (`baud`, and the `serial-lines` reset that is the default
# for a board addressed by `port`), so neither appears here.
set -euo pipefail
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/demo-run.sh" fire27 "$@"
