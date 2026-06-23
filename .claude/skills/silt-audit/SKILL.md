---
name: silt-audit
description: Loop-engineered silt codebase audit/fix cycle. Use to run an audit round (or converge the tree over several rounds) — probe for dead code, audit areas in parallel, auto-fix internal findings with locking regression tests, single build+test+commit. Invoked on demand or by the nightly cron routine.
---

# silt-audit

A self-driving audit loop for the silt language. Ported from the manual
`codebase-audit.md` prompt into a deterministic Workflow that needs no
turn-by-turn prompting.

## How to run it

The round is encoded as a Workflow at `.claude/workflows/silt-audit.js`. Run a
full convergence pass with:

```
Workflow({ name: "silt-audit" })
```

Optional `args`: `{ branch, maxRounds, ... }` (defaults: branch
`audit/auto-nightly`, `maxRounds` 6). The nightly cron routine calls exactly
this. The loop keeps running rounds until **2 consecutive clean rounds** (zero
REGRESSION and zero BROKEN found), the round cap, or the token budget runs low,
then pushes the branch and sends an ntfy completion ping to
`ntfy.sh/rendro-silt-completion-987`.

## The convergence mechanism (read this first)

**Every fix ships with a regression test that fails before the fix and passes
after.** This is the load-bearing invariant. A fix without a locking test lets
the same bug class reappear next round and the loop spins in place. With locks,
every fix is permanent and the **REGRESSION rate trends toward zero** — that is
the primary health signal. Raw BROKEN count oscillates with probe depth (a burst
after several clean rounds means agents probed deeper, not that fixes broke), so
the loop's stop condition watches REGRESSION/BROKEN==0, not the absolute count.

The lock must exercise the path the bug actually lives in. **A typecheck-only
lock for a runtime bug is a tautology** — the typechecker can auto-register
something the compiler/VM still refuses to bind. Runtime/dispatch/codegen bugs
must be locked by compiling+running via the project's runner helpers
(`run_silt_ok` / `run_silt_raw` / `run_silt_inproc` in `tests/`, or
`InProcessRunner` from `src/scheduler/test_support.rs`) and asserting on
output/exit. User-facing-string fixes prefer a source grep lock
(`include_str!` + `assert!(!src.contains("bad"))`) over a construct-and-format
lock, which is tautological.

## silt-specific constraints baked into the workflow

These shape the architecture and must not be undone:

1. **Concurrent `cargo test` runs false-fail `scheduler_deadlock_detector_tests`**
   (CPU oversubscription). Therefore fix agents NEVER run cargo; the single
   integrator runs the one authoritative `cargo build && cargo test`, serially.
   If the deadlock detector fails, re-run that one test alone before trusting it.
2. **`target/` disk blowup** has crashed the linker (175GB → lld SIGBUS). Fix
   agents work in **source-only worktrees** and never create a target dir.
3. **Single crate**, not a workspace — `cargo build`/`cargo test` is whole-crate.

Because fix agents only edit inside their own isolated worktree and return a
patch, and exactly ONE integrator owns all git history, the old doc's no-git-ops
/ shared-tree / mid-session-revert scaffolding is obsolete.

## Round shape (what the workflow does)

1. **Prep** — branch + regenerate the authoritative prior-fix log:
   `git log --grep='Fix audit findings' --format='%h %s%n%b' -80 > /tmp/prior-audit-fixes.txt`.
   Auditors grep this to avoid re-reporting and, crucially, to detect REGRESSIONs.
   Treat every prior commit's prose as a hypothesis to verify, not settled fact.
2. **Probe** — one read-only dead-code/bloat structural scan (runs first; fast,
   mechanical fixes, and reveals which files deserve the deeper correctness audit).
   Watches for parallel-array drift without a parity lock.
3. **Audit** — 6 area agents in parallel (type-system, vm-runtime, error-handling,
   dx, docs, tests), each read-only, each cross-checking the prior-fix log. Every
   finding needs a concrete repro traced from user-facing input. Zero findings is
   a valid outcome — no manufacturing problems to fill space.
4. **Synthesis** (plain code) — dedup, drop SOLID, partition REGRESSION/BROKEN and
   in-scope-vs-deferred. Out-of-scope (design-needing) findings are reported, never
   auto-fixed.
5. **Fix** — one worktree-isolated agent per in-scope finding, editing + adding
   the lock test, returning a patch. No cargo.
6. **Integrate** — one agent applies all patches, runs the single build+test,
   re-verifies each BROKEN/REGRESSION repro from the built binary, bisects out any
   patch that breaks the tree, makes ONE commit whose subject starts with
   `Fix audit findings:` (parseable by the next round), and force-pushes the branch.

## Scope of auto-fixing

IN SCOPE (internal, no user-visible surface change): internal bug fixes
(typechecker/VM/runtime/scheduler/compiler); soundness fixes that reject
previously-unsound programs; error-message/diagnostic/span improvements; missing
regression tests; doc corrections; internal refactors / dead-code removal (with a
no-op lock test); perf fixes with unchanged semantics.

OUT OF SCOPE — report and defer, do not fix: new/changed syntax/keywords/grammar;
new features; stdlib signature changes that break correctly-typed programs;
user-visible renames; changes to precedence/evaluation-order/scoping; anything
needing a design decision. Deferred items are surfaced in the final summary for a
human call.

## Commit log = memory

The git commit log is the cross-round memory — more reliable than any memory
system. Each round's commit body is one block per finding
(`<SEVERITY>: ... / File / Test / Repro`, plus `Regressed-from:` for
REGRESSIONs). The next round parses these to know what is already fixed and what
must not regress. This is why the commit format is strict.
