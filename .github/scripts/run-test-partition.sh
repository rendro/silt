#!/usr/bin/env bash
# Run one CI partition of the test suite.
#
# Usage: run-test-partition.sh <heavy|concurrency|rest1|rest2>
#
# The integration tests are grouped into suite binaries, one directory
# each under tests/ (tests/<suite>/main.rs), plus a few standalone files
# that set process-wide environment variables and so keep a process of
# their own. The partitions:
#   - heavy:        the `heavy` suite (the two large legacy files).
#   - concurrency:  the `concurrency` suite, kept apart because its tests
#                   spawn scheduler threads and are sensitive to CPU
#                   contention from other suites.
#   - rest1/rest2:  everything else, including the lib and bin unit tests,
#                   split in two by nextest's hash partitioning so the
#                   shards stay balanced as tests are added.
#
# A new test goes into the suite whose subject it tests; no list here
# needs to change.
set -euo pipefail

partition="${1:?missing partition arg: heavy|concurrency|rest1|rest2}"
runner="${SILT_TEST_RUNNER:-nextest}"
nextest=false
if [[ "$runner" == "nextest" ]] && command -v cargo-nextest >/dev/null 2>&1; then
  nextest=true
fi

rest_filter='not (binary(=heavy) | binary(=concurrency))'

set -x
case "$partition" in
  heavy)
    if $nextest; then
      exec cargo nextest run --all-features --test heavy
    else
      exec cargo test --all-features --test heavy
    fi
    ;;
  concurrency)
    # At most 4 tests at once, as .config/nextest.toml does for nextest:
    # all at once oversubscribes the CPU and the timing tests fail.
    if $nextest; then
      exec cargo nextest run --all-features --test concurrency
    else
      exec cargo test --all-features --test concurrency -- --test-threads=4
    fi
    ;;
  rest1 | rest2)
    if $nextest; then
      shard="${partition#rest}"
      exec cargo nextest run --all-features -E "$rest_filter" --partition "hash:${shard}/2"
    else
      # Without nextest there is no hash partitioning: rest1 runs
      # everything outside heavy and concurrency, rest2 nothing.
      if [[ "$partition" == "rest1" ]]; then
        suites=()
        for d in tests/*/; do
          name="$(basename "$d")"
          [[ -f "$d/main.rs" && "$name" != heavy && "$name" != concurrency ]] && suites+=("--test" "$name")
        done
        for f in tests/*.rs; do suites+=("--test" "$(basename "$f" .rs)"); done
        exec cargo test --all-features --lib --bins "${suites[@]}"
      fi
      exit 0
    fi
    ;;
  *)
    echo "unknown partition: $partition (expected: heavy|concurrency|rest1|rest2)" >&2
    exit 2
    ;;
esac
