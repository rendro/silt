#!/bin/sh
# Seed all fuzz corpora from examples/*.silt.
#
# libFuzzer cold-starts each campaign when its corpus dir is empty, so
# coverage growth is slow. Populating each target's corpus/<target>/ dir
# with real .silt sources gives the fuzzer a head start — any bytes are
# valid libFuzzer input, and .silt source exercises the lexer/parser/
# formatter/roundtrip targets on realistic shapes.
#
# Safe to re-run; cp overwrites existing files of the same name.
set -e
cd "$(dirname "$0")/.."

# Known fuzz targets — corpus dirs are created on demand so this script
# works even on a fresh clone where only fuzz/corpus/.gitkeep exists.
for target in fuzz_lexer fuzz_parser fuzz_formatter fuzz_roundtrip fuzz_typechecker fuzz_compiler fuzz_run; do
  target_dir="fuzz/corpus/$target"
  mkdir -p "$target_dir"
  cp examples/*.silt "$target_dir"/ 2>/dev/null || true
done

# fuzz_http_request reads HTTP requests, not silt sources: its seeds are
# the requests committed as fuzz/corpus/fuzz_http_request/*.http, and
# there is nothing to copy for it.

echo "Seeded corpora from examples/ into fuzz/corpus/<target>/"
