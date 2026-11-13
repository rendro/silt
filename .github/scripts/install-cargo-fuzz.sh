#!/usr/bin/env bash
# Install cargo-fuzz at a fixed version: the prebuilt x86_64 Linux binary
# of its GitHub release, refused unless its SHA-256 is the one below.
# The fuzz jobs of ci.yml and fuzz-nightly.yml run it.
#
# (Building it from crates.io took whatever version was newest that day,
# with fresh dependencies: its published Cargo.lock pins a rustix that
# current nightlies refuse, so a locked build was no way out.)
#
# To move to a newer cargo-fuzz: set the version, download its
# cargo-fuzz-<version>-x86_64-unknown-linux-musl.tar.gz from
# https://github.com/rust-fuzz/cargo-fuzz/releases, and set its
# `sha256sum` here.
set -euo pipefail

version=0.13.2
sha256=b5b704018b63e0f151c17a057ac53b5111e1db545d1b9f72fee79f08a545931c

archive="cargo-fuzz-$version-x86_64-unknown-linux-musl.tar.gz"
dir=$(mktemp -d)
curl --proto '=https' --tlsv1.2 -fsSL -o "$dir/$archive" \
  "https://github.com/rust-fuzz/cargo-fuzz/releases/download/$version/$archive"
echo "$sha256  $dir/$archive" | sha256sum -c -
tar xzf "$dir/$archive" -C "$dir" cargo-fuzz
bin="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$bin"
install -m 755 "$dir/cargo-fuzz" "$bin/cargo-fuzz"
rm -rf "$dir"
cargo fuzz --version
