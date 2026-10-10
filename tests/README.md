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
cargo nextest run --all-features --test lang          # lang, meta, typecheck, cli, lsp, frontend, heavy, oracle, concurrency
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
| `SILT_FMT_CORPUS_ALL=1` | (`heavy`) every file under that directory, whatever its name (a fuzz corpus) |
| `SILT_FMT_WORKERS=<n>` | (`heavy`) the number of threads (default: 2, and every CPU for the full sweep) |
| `SILT_FMT_REPORT=<file>` | append the counts and every failure to the file |
| `SILT_FMT_FAILED=<dir>` | keep each input that failed as a file in the directory |
| `SILT_FMT_STRESS=<count>` | (`heavy`) run `random_comments`: that many inputs with 2 to 12 comments each at random sites |
| `SILT_FMT_SEED=<n>` | (`heavy`) the seed of those random sites (default 1) |
| `SILT_FMT_ONLY=<text>` | only the inputs whose name holds the text: one file with every mutant of it (`SILT_FMT_ONLY=fmt/printer__pipelines SILT_FMT_FULL=1`) |

A passing test prints nothing under nextest; add `--success-output
immediate` to see the counts, or read the report file.

No input may fail: a refusal is a defect of the formatter.

The runner also checks that the comments that start a file, up to the
first empty line or declaration, are the first bytes of the result: the
golden harness reads a case's directives there.

The full sweep, with other corpora (a fuzz corpus, generated mutants)
added from outside the tree, and the random comments, in release:

```
SILT_FMT_FULL=1 SILT_FMT_CORPUS=<dir> SILT_FMT_REPORT=/tmp/fmt.txt \
  cargo test --release --all-features --test heavy every_input -- --nocapture
SILT_FMT_STRESS=200000 cargo test --release --all-features --test heavy random_comments -- --nocapture
```

## The differential oracle

`tests/oracle` takes programs that check clean and whose code names no
builtin that reaches outside the VM (files, the environment, the
network, arguments, stdin), compiles each, verifies every compiled
function again, and runs it twice, each run on a VM of its own with its
output in a buffer. The two runs must agree in output, in the reports
of failed tasks and in `main`'s value or error, and no run may end in a
`type_confusion` error, an internal error or a panic
(`tests/oracle/oracle.rs`). A program that uses tasks, channels,
streams, the clock or the system's random source is compared only where
a golden case's exact `.stdout` says what it writes.

Its inputs: the golden cases that are run and must end with status 0.

```
cargo nextest run --all-features --test oracle                      # a sample of each class
SILT_ORACLE_FULL=1 cargo nextest run --all-features --test oracle   # every input
```

| Variable | Meaning |
|---|---|
| `SILT_ORACLE_FULL=1` | every input of a class instead of its sample |
| `SILT_ORACLE_ONLY=<text>` | only the inputs whose name holds the text |
| `SILT_ORACLE_WORKERS=<n>` | the number of threads (default: 2) |
| `SILT_ORACLE_REPORT=<file>` | append the counts, every finding and the verdict of each input to the file |

A finding is a defect of silt, not of the input. One that is known and
reported has a line in `tests/oracle/skip.txt` that names it; the suite
fails on a finding without a line and on a line whose input has no such
finding any more, so the line goes with the fix. The inputs the file
names are part of every sample.

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
