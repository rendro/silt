# Running the tests

## The whole suite

```
cargo nextest run --all-features --no-fail-fast
cargo test --all-features --doc
```

[cargo-nextest](https://nexte.st) runs the test binaries in parallel and
is what CI uses. `.config/nextest.toml` caps the `concurrency` suite at
four threads; its tests assert on timing and fail when the machine is
oversubscribed. With plain `cargo test`, run that suite on its own:
`cargo test --all-features --test concurrency -- --test-threads=4`.

Never run two test runs at once on one machine: the timing tests of the
`concurrency` suite fail under load.

## One suite, one area

```
cargo nextest run --all-features --test lang          # lang, meta, typecheck, cli, lsp, frontend, heavy, concurrency
SILT_GOLDEN_FILTER=lang/traits/ cargo nextest run --all-features --test golden
```

The golden cases and their format are described in `golden/README.md`.
The full repro corpus runs with
`SILT_GOLDEN_FULL_CORPUS=1 cargo nextest run --all-features --test golden -E 'test(verdict_shard)'`.

## A faster local build

These go in `~/.cargo/config.toml`, not in the repository (CI does not
need them):

```toml
[build]
rustc-wrapper = "sccache"          # dependencies compile once for every checkout and worktree

[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=mold"]   # a faster linker for the test binaries

[profile.dev]
debug = "line-tables-only"         # backtraces keep file and line; smaller objects
```

On a four-core machine this takes a fresh checkout's test build from
about 155 s to 55 s and the full suite to under five minutes.
