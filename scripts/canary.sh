#!/bin/sh
# The live canary: the real agents run one trivial prompt each in a spandeck
# pane. Spends a little quota. Run nightly on a station that is logged in.
set -e
cd "$(dirname "$0")/.."
cargo test -p spandeck --test phase3 --test phase7 -- --ignored canary
cargo test -p spandeck --test phase5 -- --ignored live_tailscale
