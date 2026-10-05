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

## The formatter's property runner

`tests/frontend/fmt_property/mod.rs` formats each input and checks that
the formatter did not refuse it, that the result parses and holds the
input's comments, and that a second pass changes nothing. Its inputs are
the examples, the docs' `silt` snippets, the golden cases, the fuzz
corpus `fuzz/corpus/fuzz_formatter`, and comment mutants made in the
test: an input with one `--` or `{- -}` comment in one gap between two
tokens.

```
cargo nextest run --all-features --test frontend -E 'test(fmt_property)'   # the examples and a sample of their mutants
cargo nextest run --all-features --test heavy -E 'test(fmt_property)'      # every input, a sample of the mutants
```

| Variable | Meaning |
|---|---|
| `SILT_FMT_FULL=1` | (`heavy`) a mutant for every gap of every example, snippet and golden case |
| `SILT_FMT_CORPUS=<dir>` | (`heavy`) also every `.silt` file under the directory |
| `SILT_FMT_REPORT=<file>` | append the counts and every failure to the file |

A passing test prints nothing under nextest; add `--success-output
immediate` to see the counts, or read the report file.

Both runs are marked `Expect::KnownFailing`: today's `silt fmt` refuses
many of the mutants, so the tests report the count and pass. The step
that replaces the formatter changes the marks to `Expect::Clean`.

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
