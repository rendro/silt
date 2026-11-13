#!/usr/bin/env bash
# Build the playground's module into playground/pkg: `silt_playground.js`
# (an ES module) and `silt_playground_bg.wasm`, with their type
# declarations. Needs wasm-pack and the target wasm32-unknown-unknown.
set -euo pipefail
cd "$(dirname "$0")"
rm -rf pkg
# `--locked`: the module is built from the versions in Cargo.lock.
wasm-pack build --release --target web --out-dir pkg --no-pack -- --locked
# wasm-pack's own ignore file would keep the files out of an archive.
rm -f pkg/.gitignore
