# Fuzzing silt

silt ships thirteen libfuzzer targets:

| Target             | Exercises                                          |
|--------------------|----------------------------------------------------|
| `fuzz_lexer`       | `Lexer::tokenize` on any text, lex errors included + span/offset invariants |
| `fuzz_parser`      | the parser on any tokens, the lexer's Error tokens included (must not panic) + AST invariants of what parses |
| `fuzz_formatter`   | `format::format`: never refused, a fixed point     |
| `fuzz_roundtrip`   | parse → format → parse (must preserve structure)   |
| `fuzz_typechecker` | the session's analysis + diagnostic well-formedness |
| `fuzz_compiler`    | `Compiler::compile_program` on clean programs + disassembly decode |
| `fuzz_http_request` | the reader of `http.serve`'s requests (`src/http_wire.rs`): no panic, no request beyond a limit, the same requests however the bytes arrive |
| `fuzz_decode`      | the decoder on any text (`src/fuzz_invariants.rs`): `json.parse`, `toml.parse`, `toml.parse_map` and `toml.parse_list` into each of a set of types, with and without written impls of a fixed set, as the VM's frame runs a decoding: no panic, the same result again, and `json.decode` of a JSON document's `json.Value` decodes alike (value or error, the impls' calls in order); the JSON and TOML text the input's bytes choose too |
| `fuzz_package_files` | the readers of `silt.toml` and `silt.lock` on any text (`src/fuzz_invariants.rs`): no panic, an error's place inside the text, and a lockfile that reads is written to a text that reads back as the same lockfile and is written alike again |
| `fuzz_csv`         | the csv reader of `csv.parse` and `csv.split_line` on any text (`src/fuzz_invariants.rs`): no panic, an error's place inside the text, and records that read, written again with every field quoted, read back the same |
| `fuzz_http_response` | the client's reading of a response (`http.get`, `http.request`; `src/fuzz_invariants.rs`): any bytes as a response's head and body become a `Response` without a panic, with the status, headers and body sent (a gzip body unpacked, nothing to a `HEAD`), and a redirect goes to a URL without a fragment, on the same server for a path, with no `.` or `..` left |
| `fuzz_lsp_messages` | `silt lsp` on any bytes from the editor (`src/fuzz_invariants.rs`): the framed messages read the same however the bytes arrive, and the server handles each without a panic and answers each request it handled once |
| `fuzz_run`         | the differential oracle (`tests/oracle/oracle.rs`): a program that checks clean and stays inside the VM is run in slices of 2000 steps and of one step under a step budget; the runs agree, and none ends in a type confusion, an internal error or a panic |

Invariant helpers live in `src/fuzz_invariants.rs` and are exercised
from both the fuzz targets and regression tests in `tests/`.

`fuzz_run` is the one target that runs programs. It shares its code
with the `oracle` test suite (see "The differential oracle" in
`tests/README.md`), which runs the same checks over the golden cases,
generated programs and the corpora on every push. A finding of
`fuzz_run` is a defect of the checker or the runtime: minimise it,
commit it as `fuzz/corpus/fuzz_run/<name>.silt`, and the suite's corpus
class runs it too (list it in `tests/oracle/skip.txt` until it is
fixed). Its seeds `run_*.silt` are small programs of one area each
(closures, traits, lists, tasks and channels, streams, timers, a
runtime error); the suite fails when one of them is not run.

## Three-lane workflow

silt splits fuzz work across three lanes so that CI stays fast and
deterministic while real discovery still happens.

### 1. Blocking CI — corpus replay (`ci.yml` → `fuzz-corpus` job)

Every push and every PR runs `fuzz/replay.sh <target>` for each
target. `replay.sh` replays every committed file under
`fuzz/corpus/<target>/` through the target binary **exactly once,
with no mutation**. The job takes a few seconds per target and is
100 % deterministic — there is no way for it to fail flakily.

This gate catches regressions on any bug we have already found.
Every crash input we ever add to `fuzz/corpus/<target>/` becomes a
permanent part of this regression suite.

### 2. Nightly CI — deep discovery (`fuzz-nightly.yml`)

A scheduled GitHub Actions workflow fires daily at 06:00 UTC and
runs 15 minutes of real libfuzzer mutation per target (configurable
up to the runner wall limit via `workflow_dispatch`). If a target
crashes, the reproducer is uploaded as an artifact (30-day retention)
and its job fails. Each failed job of a scheduled run opens a GitHub
issue, "nightly: <job> failed" (a later failure is added to it while it
is open). Nightly failures do **not** block merges, but they block a
release: `release.yml` refuses to start while the newest finished
nightly run on main is not green. Fix the cause, or run the workflow
again until it is green.

Trigger it on-demand with the "Run workflow" button if you want a
deep sweep after a big parser or formatter change.

### 3. Local — active discovery (`fuzz/local.sh`)

While actively working on parser or formatter code, run

```sh
fuzz/local.sh fuzz_formatter       # single target, 10 min
fuzz/local.sh fuzz_formatter 60    # quick 1-minute sanity check
fuzz/local.sh all 3600             # every target in parallel, 1 hr
```

This is the fastest way to find new bugs — seconds of local
feedback versus minutes of CI round-trip — and it parallelises
across all CPU cores when invoked with `all`.

## Lifecycle of a fuzz-found bug

When a local run or the nightly job surfaces a new crash:

1. **Reproduce it.** `cargo +nightly fuzz run <target> path/to/crash`
2. **Minimise.** `cargo +nightly fuzz tmin <target> path/to/crash` —
   produces a minimized input under `fuzz/artifacts/<target>/`.
3. **Lock it.** Copy the (ideally minimised) input to
   `fuzz/corpus/<target>/<short-name>.silt`. The corpus-replay gate
   will now run it on every push — the bug can never silently
   regress once it is fixed.
4. **Write a unit test.** Add a test under `tests/` that constructs
   the minimised pattern from string literals and asserts the fixed
   behaviour directly. A corpus seed locks the fuzz target; a unit
   test locks the fix with a human-readable description.
5. **Fix it.** Land the fix and the two tests together.

## Running the corpus replay locally

Exactly the same script CI uses:

```sh
fuzz/replay.sh fuzz_formatter
```

Use it as a pre-push sanity check — if it fails locally, it will
fail in CI.

## Seeding new corpora

`fuzz/seed.sh` copies every file from `examples/` into each
target's corpus directory. Re-run it after adding a new example
so the fuzzer gets the wider coverage on the next round.
