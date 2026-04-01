# Silt Roadmap — Final Status

## Context

The EVALUATION.md report scored silt 5/10 for production readiness. This roadmap organized fixes into 5 phases across correctness, stdlib, type system, performance, and tooling. All 5 phases have been implemented.

## Decisions (Resolved)

1. **Numeric tower** → **A: Remove implicit coercion.** Users write `int.to_float(3) + 2.5`.
2. **Cross-type comparison** → **A: Restrict all comparisons to same-type at runtime.**
3. **Builtin architecture** → **C: Split into per-module functions.** Both interpreter and typechecker.
4. **Concurrency** → **A: Stay single-threaded, add preemptive yielding.**
5. **Exhaustiveness** → **A: Maranget usefulness algorithm.**

---

## Implementation Summary

### Phase 1: Correctness (5/10 → 6/10) — Complete

| Item | Status | Divergence |
|------|--------|------------|
| 1.1 Remove int/float coercion | Done | — |
| 1.2 Same-type comparisons | Done | — |
| 1.3 `silt check` command | Done | — |
| 1.4 Runtime source locations | Done | Call stack traces added in polish pass |
| 3.3 Fix fresh_var fallback | Done | Pulled forward from Phase 3 |

### Phase 2: Stdlib & Usability (6/10 → 7/10) — Complete

| Item | Status | Divergence |
|------|--------|------------|
| 2.1 Split dispatch_builtin | Done | — |
| 2.2 Math module (13 functions) | Done | `math.pi`/`math.e` registered as values; "math" not in BUILTIN_MODULES to avoid user module conflicts |
| 2.3 map.filter/map/entries/from_entries | Done | — |
| 2.4 list.group_by | Done | — |
| 2.5 REPL (rustyline) | Done | `:type` deferred; added `:env`, tab completion instead |

### Phase 3: Type System (7/10 → 8/10) — Complete

| Item | Status | Divergence |
|------|--------|------------|
| 3.1 Maranget exhaustiveness | Done | Harder than estimated; tuple column decomposition required iterative debugging. Also handles Record types (single-constructor). |
| 3.2 Trait dispatch validation | Done | Pragmatic `trait_impls.iter().any()` check, not full trait registry |

### Phase 4: Performance (8/10 → 8.5/10) — Complete

| Item | Status | Divergence |
|------|--------|------------|
| 4.1 List/map/record COW | Done | Expanded scope: also covers map.set/delete/merge and RecordUpdate |
| 4.2 Env flattening | Reverted | Flattening caused excessive memory for deep recursion (copying ~120 builtins per child scope). Reverted to parent-chain O(d) lookup. |
| 4.3 Preemptive yielding | Done | Simpler than planned: no task state save/resume, just periodic `run_pending_tasks_once()` |
| 4.4 Rendezvous channels | Doc only | True rendezvous incompatible with tree-walking architecture |

### Phase 5: Tooling — Complete

| Item | Status | Divergence |
|------|--------|------------|
| 5.1 Comment-preserving formatter | Done | Source-scanning approach (not lexer trivia tokens). Preserves standalone comment lines between declarations. |
| 5.2 Parser error recovery | Done | `parse_program_recovering` + synchronize at declaration boundaries |
| 5.3 JSON diagnostics | Done | `silt check --format json` |

### Polish Pass — Complete

| Item | Status |
|------|--------|
| Call stack traces on runtime errors | Done |
| REPL tab completion | Done |
| Typechecker register_builtins split | Done |
| Fix programs broken by stricter typechecker | Done (test_suite.silt, text_stats.silt) |
| Fix Record exhaustiveness (single-constructor) | Done |
| Fix Record trait method resolution | Done |

---

## Final Numbers

- **Tests:** 350 (was 323)
- **Code:** +2700 lines across 14 files
- **New dependency:** rustyline 15
- **Programs verified:** 8/10 run clean (2 have pre-existing issues: expr_eval stack overflow, csv_analyzer parse bug)

## Remaining Items (Deferred)

| Item | Effort | Why deferred |
|------|--------|-------------|
| REPL `:type` command | M | Needs standalone expression type inference entry point |
| Inline comment preservation in formatter | M | Requires token-level trivia tracking |
| True rendezvous channels | XL | Incompatible with tree-walking interpreter architecture |
| Full trait registry | L | Pragmatic string-based check works for current programs |
| Env optimization (non-flattening) | M | Parent-chain is fine for typical depth; flattening caused regression. Indexed addressing or caching would be the next approach. |
