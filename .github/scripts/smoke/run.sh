#!/usr/bin/env bash
# run.sh SILT VERSION [RUNNER...]
#
# The smoke run of a release binary before it is published
# (release.yml): `SILT --version` must print `silt VERSION`, and
# smoke.silt beside this file must print smoke.stdout. RUNNER, when
# given, is the command the binary runs under (`arch -x86_64` for the
# x86_64 macOS binary on an ARM runner).
set -euo pipefail

silt=$1
version=$2
shift 2
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"

got=$("$@" "$silt" --version | tr -d '\r')
if [ "$got" != "silt $version" ]; then
  echo "::error::$silt --version printed \"$got\", not \"silt $version\""
  exit 1
fi

got=$("$@" "$silt" run smoke.silt | tr -d '\r')
want=$(tr -d '\r' < smoke.stdout)
if [ "$got" != "$want" ]; then
  echo "::error::smoke.silt printed something else than smoke.stdout"
  printf 'printed:\n%s\nexpected:\n%s\n' "$got" "$want"
  exit 1
fi
echo "$silt: $got"
