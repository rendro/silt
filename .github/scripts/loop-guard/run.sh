#!/usr/bin/env bash
# run.sh SILT
#
# The guard of the instruction loop's code. SILT is a release binary of
# silt, built as the release job builds it. For each of the two loops
# beside this file the binary is started, stopped in the loop, and
# stepped for 200,000 instructions; then the round of the loop must
# have
#   - no load that cannot be forwarded from the store buffer, and
#   - no more machine instructions than the bound below.
# See README.md here for what that is and why it is checked.
#
# Needs: Linux on x86_64, a C compiler (`cc`), python3, binutils (`nm`,
# `objdump`), and leave to ptrace a child.
set -euo pipefail

silt=$(realpath "$1")
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cc -O2 -o "$work/step" "$here/step.c"

# The bounds: about 8% above what rustc 1.99.0 gave on 2026-10-12
# (557 and 787 instructions per round).
failed=0
for row in "plain_loop 600" "add_round 850"; do
  set -- $row
  name=$1 bound=$2
  # 300 ms after its start the program is in its loop; 200,000
  # instructions are stepped, the registers of 40,000 of them kept.
  "$work/step" "$work/$name.counts" "$work/$name.trace" 300 200000 40000 \
    "$silt" run "$here/$name.silt"
  python3 "$here/count.py" "$silt" "$work/$name.counts" "$work/$name.trace" | tee "$work/$name.out"
  result=$(tail -n 1 "$work/$name.out")
  instructions=$(echo "$result" | awk '{print $2}')
  loads=$(echo "$result" | awk '{print $4}')
  echo "$name: $instructions instructions per round (at most $bound), $loads loads per round that cannot be forwarded (none allowed)"
  if awk -v i="$instructions" -v b="$bound" -v l="$loads" 'BEGIN { exit !(i > b || l > 0.05) }'; then
    echo "$name: FAILED"
    failed=1
  fi
done
exit $failed
