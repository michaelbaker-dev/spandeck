#!/bin/sh
# The every-change tier: format, lint, and the tests. Budget: under 90 seconds.
# Usage: scripts/check.sh [crate…]   (no arguments: the whole workspace)
#
# The whole workspace's test files are run several at a time (up to four;
# SPANDECK_CHECK_JOBS says another number, and 1 is the old way). One after
# another, as `cargo test --workspace` runs them, they took 80 seconds, nearly
# all of it waiting: a file takes as long as its slowest test, and the slowest
# tests wait for a station's own clocks (a nudge, a look at the fleet, a stop
# that cannot be delivered). The same tests are run, each file by the same
# `cargo test` as before; only the order and the output changed. What a file
# printed is shown when it fails.
set -e
cd "$(dirname "$0")/.."
start=$(date +%s)
cargo fmt --all -- --check
if [ $# -eq 0 ]; then
  cargo clippy --workspace --all-targets --quiet -- -D warnings
  # Built once, so that the runs below find nothing to build and do not wait
  # for each other.
  cargo test --workspace --quiet --no-run
  logs=$(mktemp -d)
  trap 'rm -rf "$logs"' EXIT
  {
    # Every crate's unit tests, then its documentation's.
    echo "--workspace --lib --bins"
    echo "--workspace --doc"
    # Then each file of tests, the largest first: a large file is a slow
    # one more often than not, and the slow ones are best started early.
    # Asked of the whole workspace, as the build above was: asked of its
    # own crate (`-p`), a file is built a second time, with that crate's
    # choice of features. (A name two crates both have is run once, for
    # both.)
    for f in $(ls -S crates/*/tests/*.rs); do
      basename "$f" .rs
    done | awk '!seen[$0]++ { print "--workspace --test " $0 }'
  } > "$logs/list"
  # How many at a time: one for every four processors, four at most (a
  # test file already runs its own tests one to a processor), so a small
  # machine runs them one after another as before.
  cpus=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)
  jobs=$(( cpus / 4 ))
  [ "$jobs" -ge 1 ] || jobs=1
  [ "$jobs" -le 4 ] || jobs=4
  # One line of the list is one run (its words are the run's arguments). A
  # run that fails prints what it said; the others still run, and xargs
  # then fails this script.
  # shellcheck disable=SC2016
  xargs -P "${SPANDECK_CHECK_JOBS:-$jobs}" -L 1 sh -c '
    log=$(mktemp "$0/run.XXXXXX")
    if cargo test --quiet "$@" > "$log" 2>&1; then
      exit 0
    fi
    { echo "---- cargo test $*"; cat "$log"; } >&2
    exit 1
  ' "$logs" < "$logs/list"
  # What `cargo test --workspace --quiet` said in a line per program, in one.
  cat "$logs"/run.* | awk '/^test result: ok\./ { programs += 1; passed += $4; ignored += $8 }
    END { printf "%d tests passed and %d were ignored, in %d test programs\n", passed, ignored, programs }'
else
  for c in "$@"; do
    cargo clippy -p "$c" --all-targets --quiet -- -D warnings
    cargo test -p "$c" --quiet
  done
fi
took=$(( $(date +%s) - start ))
echo "check passed in ${took}s (budget: 90s)"
[ "$took" -le 90 ] || { echo "over the 90 second budget: delete tests or move them to nightly" >&2; exit 1; }
