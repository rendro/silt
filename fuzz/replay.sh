#!/bin/sh
# Deterministic corpus replay for one fuzz target.
#
# This is the blocking-CI fuzz job: it replays every file in
# fuzz/corpus/<target>/ through the fuzz target binary exactly once,
# with NO mutation. Contrast with `cargo fuzz run <target> --
# -max_total_time=N`, which mixes corpus replay with random mutation
# and therefore finds a different bug on each run — a bad fit for
# gating merges.
#
# Arguments: <target-name>
# Exit: 0 if every corpus file runs clean; non-zero (libfuzzer's 77)
# if any file triggers a panic or invariant violation.
#
# See fuzz/README.md for the broader workflow (local discovery,
# nightly deep fuzz).

set -e
cd "$(dirname "$0")/.."

target="${1:-}"
if [ -z "$target" ]; then
  echo "usage: fuzz/replay.sh <fuzz_target>" >&2
  exit 1
fi

corpus_dir="fuzz/corpus/$target"
if [ ! -d "$corpus_dir" ]; then
  echo "replay.sh: no corpus dir at $corpus_dir" >&2
  exit 1
fi

# Build the target binary once via cargo-fuzz, then invoke it
# directly with corpus files as args. libfuzzer treats explicit file
# args as "reproduce these inputs and exit" — exactly the mode we
# want.
echo "replay.sh: building $target"
cargo +nightly fuzz build "$target"

bin="fuzz/target/x86_64-unknown-linux-gnu/release/$target"
if [ ! -x "$bin" ]; then
  echo "replay.sh: target binary not found at $bin" >&2
  exit 1
fi

count=$(find "$corpus_dir" -type f -name '*.silt' | wc -l)
if [ "$count" -eq 0 ]; then
  echo "replay.sh: corpus for $target has no .silt seeds — nothing to replay"
  exit 0
fi

echo "replay.sh: replaying $count .silt seeds from $corpus_dir"

# Only replay committed .silt seeds — fuzz/corpus/.gitignore ignores
# libfuzzer-generated hash-named files so a local `fuzz/local.sh`
# session does not explode the replay time. 500 per xargs batch
# keeps each libfuzzer launch under a few seconds.
find "$corpus_dir" -type f -name '*.silt' -print0 | xargs -0 -n 500 "$bin"

echo "replay.sh: $target corpus replay OK ($count files)"
