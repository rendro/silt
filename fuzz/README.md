# Fuzzing silt

silt ships four libfuzzer targets:

| Target           | Exercises                                          |
|------------------|----------------------------------------------------|
| `fuzz_lexer`     | `Lexer::tokenize` + span/offset invariants         |
| `fuzz_parser`    | `Parser::parse_program` (must not panic)           |
| `fuzz_formatter` | `formatter::format` + round-trip invariants        |
| `fuzz_roundtrip` | parse → format → parse (must preserve structure)   |

Invariant helpers live in `src/fuzz_invariants.rs` and are exercised
from both the fuzz targets and regression tests in `tests/`.

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
and the workflow run fails. Nightly failures do **not** block
merges; they show up as a bright signal on the Actions tab.

Trigger it on-demand with the "Run workflow" button if you want a
deep sweep after a big parser or formatter change.

### 3. Local — active discovery (`fuzz/local.sh`)

While actively working on parser or formatter code, run

```sh
fuzz/local.sh fuzz_formatter       # single target, 10 min
fuzz/local.sh fuzz_formatter 60    # quick 1-minute sanity check
fuzz/local.sh all 3600             # all four targets in parallel, 1 hr
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
