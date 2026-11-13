#!/bin/sh
# The every-change tier: format, lint, and the tests. Budget: under 90 seconds.
# Usage: scripts/check.sh [crate…]   (no arguments: the whole workspace)
set -e
cd "$(dirname "$0")/.."
start=$(date +%s)
cargo fmt --all -- --check
if [ $# -eq 0 ]; then
  cargo clippy --workspace --all-targets --quiet -- -D warnings
  cargo test --workspace --quiet
else
  for c in "$@"; do
    cargo clippy -p "$c" --all-targets --quiet -- -D warnings
    cargo test -p "$c" --quiet
  done
fi
took=$(( $(date +%s) - start ))
echo "check passed in ${took}s (budget: 90s)"
[ "$took" -le 90 ] || { echo "over the 90 second budget: delete tests or move them to nightly" >&2; exit 1; }
