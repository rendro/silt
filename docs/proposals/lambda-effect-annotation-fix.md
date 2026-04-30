---
title: "Proposal: fix EffectSet::TOP collision in lambda fn-expression"
section: "Proposals"
status: draft
---

# Fix `EffectSet::TOP` collision in lambda `fn(...)` expression

**Status:** draft. Sibling fix to round-62 audit cluster B1/B2/B3 (commit
`5b3f240`), which closed the same root-cause bug for top-level `FnDecl`
but left the lambda surface unaffected.

**Scope:** disambiguate "user wrote no effect annotation" from "user
wrote `!{io, fs, net, time, random}`" on `ExprKind::Lambda`. No syntax
change. No semantic change for code that does not reach the collision.

## Background

`EffectSet::TOP` is the bitset `0b00011111` — every effect bit set. The
typechecker uses TOP as the gradual-rollout permissive default ("we
don't know, allow anything"), but it is bit-identical to the value the
parser produces when a user explicitly writes all five effects. Round 62
fixed this for `FnDecl` by adding a sibling `is_annotated: bool` field
that distinguishes the two cases. The same fix shape is needed for
lambda `fn(...)` expressions.

silt has two lambda forms:

1. **Trailing closure:** `{ x, y -> body }`. No syntactic slot for an
   effect annotation; parser unconditionally fills `EffectSet::TOP`
   (`src/parser.rs:2997`). Not affected by this proposal — there is no
   user input to disambiguate.

2. **Fn expression:** `fn(x, y) !{io} { body }`. Parser calls
   `parse_effect_annotation_opt` at `src/parser.rs:3227`, the same
   helper that round 62 split into the explicit `_opt_explicit` form
   for `FnDecl`. Lambda still uses the legacy boolean-less form, so
   the resulting `EffectSet::TOP` could mean either "no annotation"
   or the explicit five-effect set.

## Reproduction (current state)

```silt
let f = fn() !{io, fs, net, time, random} { println("hi") }
```

The parsed `ExprKind::Lambda` has `effects: EffectSet::TOP`. Downstream:

- **`src/formatter.rs:5429`** suppresses effect emission on lambdas
  when `effects == EffectSet::TOP`. `silt fmt` rewrites the example to
  `let f = fn() { println("hi") }`, silently dropping the annotation.
- **Strict-effects flip pass** treats the lambda as un-annotated and
  flips its declared set to `EMPTY`. Body inference then errors on
  the call to `println`.
- **`format_suggested_fn_header`-equivalent** for lambdas (if any) would
  suggest `fn() !*` — un-parseable, same as B3.

## Fix

Add `is_annotated: bool` to `ExprKind::Lambda` in `src/ast.rs`.
Pivot every TOP-equality site to `is_annotated`. Mirror the round-62
work in `FnDecl`. Specifically:

1. **AST:** extend `ExprKind::Lambda { params, body, effects }` with
   `is_annotated: bool`.
2. **Parser:** `parse_fn_expr` at `src/parser.rs:3221-3238` calls
   `parse_effect_annotation_opt_explicit` (already added by round 62
   for FnDecl) instead of `parse_effect_annotation_opt`. Wire the
   boolean. Trailing-closure form (`parse_trailing_closure_as_lambda`
   at `src/parser.rs:2966`) sets `is_annotated: false` unconditionally.
3. **Formatter:** `src/formatter.rs:5429` pivots on `is_annotated`
   instead of bit-equality. When `is_annotated && effects ==
   EffectSet::TOP`, emit the explicit `!{fs, io, net, random, time}`
   form (mirrors round-62 FnDecl behaviour).
4. **Typechecker:** any strict-effects flip / body-subset check that
   consumes `lambda.effects` must pivot on `is_annotated` (search:
   `rg 'Lambda \{' src/typechecker/`).

## Locking tests

In `tests/effect_lambda_round_trip_tests.rs` (new file):

- `lambda_with_all_five_effects_annotation_survives_fmt_round_trip`
  — `let f = fn() !{io, fs, net, time, random} { println("hi") }`,
  fmt, assert output still contains `!{` and the five effect names.
- `strict_effects_does_not_mistake_full_lambda_annotation_for_pure`
  — `silt check --strict-effects` on the same input, assert no error
  about lambda being declared pure.
- `lambda_unannotated_under_strict_effects_still_flips_to_empty`
  — `let f = fn() { println("hi") }` under strict-effects, assert
  the existing flip still fires (lock the regression boundary).

## Effort estimate

~2 hours. The work is mechanical: round 62 already paved the path
for `FnDecl`. The risk is missing a TOP-equality site in the
typechecker — `rg 'effects.*TOP\|TOP.*effects' src/typechecker/`
should be exhaustive.

## Out of scope

- New lambda syntax. The trailing-closure form `{ x, y -> body }`
  intentionally has no effect-annotation slot; this proposal does
  not propose adding one. Users who need lambda effect annotations
  must use the `fn(...)` form, same as today.
- Generalising `EffectSet::TOP` away. The TOP sentinel is still useful
  as a gradual-rollout default; `is_annotated` distinguishes intent
  without removing the sentinel. A larger redesign that types
  `declared_effects: Option<EffectSet>` instead is left for a future
  round.
