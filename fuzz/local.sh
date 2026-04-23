#!/bin/sh
# Discovery-mode fuzz runner for local development.
#
# Runs real libfuzzer mutation against one target (or all targets in
# parallel) for a configurable wall time. Finds NEW bugs that the
# committed corpus hasn't seen yet. Intended to be run at a desk
# during active parser/formatter work, not from CI.
#
# Usage:
#   fuzz/local.sh <target>            # single target, 10 min
#   fuzz/local.sh <target> <seconds>  # single target, custom time
#   fuzz/local.sh all                 # all targets in parallel, 10 min
#   fuzz/local.sh all 3600            # all targets in parallel, 1 hour
#
# When a crash fires, libfuzzer writes it to
# fuzz/artifacts/<target>/crash-<hash>. Follow the workflow in
# fuzz/README.md: minimize, add to corpus, file a bug, fix it.

set -e
cd "$(dirname "$0")/.."

target="${1:-}"
seconds="${2:-600}"

if [ -z "$target" ]; then
  echo "usage: fuzz/local.sh <target|all> [seconds]" >&2
  echo "targets: fuzz_lexer fuzz_parser fuzz_formatter fuzz_roundtrip" >&2
  exit 1
fi

run_one() {
  t="$1"
  s="$2"
  echo "[$t] fuzzing for ${s}s"
  cargo +nightly fuzz run "$t" -- -max_total_time="$s"
}

if [ "$target" = "all" ]; then
  # Each target gets its own background process; wait for all.
  # libfuzzer defaults to one worker per invocation; it saturates one
  # core. Running four in parallel saturates four cores, which is the
  # intended "use the whole machine" behaviour.
  for t in fuzz_lexer fuzz_parser fuzz_formatter fuzz_roundtrip; do
    run_one "$t" "$seconds" &
  done
  wait
else
  run_one "$target" "$seconds"
fi
