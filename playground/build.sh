#!/usr/bin/env bash
# Build the playground's module into playground/pkg: `silt_playground.js`
# (an ES module) and `silt_playground_bg.wasm`, with their type
# declarations. Needs wasm-pack and the target wasm32-unknown-unknown.
set -euo pipefail
cd "$(dirname "$0")"
rm -rf pkg
# The module is built from the versions in Cargo.lock. wasm-pack reads
# the crate with a `cargo metadata` of its own, without --locked, which
# would rewrite a stale lock before the locked build sees it: so the lock
# is checked first, and the build fails if the lock changed all the same.
cargo metadata --locked --format-version 1 > /dev/null
before="$(cksum < Cargo.lock)"
wasm-pack build --release --target web --out-dir pkg --no-pack -- --locked
if [ "$(cksum < Cargo.lock)" != "$before" ]; then
    echo "error: building the module changed playground/Cargo.lock" >&2
    exit 1
fi
# wasm-pack's own ignore file would keep the files out of an archive.
rm -f pkg/.gitignore
