#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Build a demo for the CoreS3, ensure the board runs it, capture its output.
#
#   tools/cores3-run.sh display              # build, ensure, capture 15 s
#   tools/cores3-run.sh display 30
#   tools/cores3-run.sh display 30 --until 'wifi: got ip'
#
# The board kind, and nothing else: everything this does lives in demo-run.sh,
# so the two boards cannot drift apart. Environment variables are documented
# there.
set -euo pipefail
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/demo-run.sh" cores3 "$@"
