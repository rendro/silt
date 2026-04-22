---
title: "Proposal: `when let ... else match` for exhaustive else branches"
section: "Proposals"
status: draft
---

# `when let ... else match`

**Status:** proposal, not yet implemented.
**Scope:** a syntactic extension to `when let` that lets the else
branch pattern-match on the scrutinee without requiring an
unreachable arm for the primary pattern. No new type-system
machinery. No flow-sensitive narrowing.

## Problem

Today a user who wants to early-exit with specific handling per
failure variant writes:

```silt
let res = load_data(path)
when let Ok(data) = res else {
  match res {
    Err(e) -> panic("load failed: {e.message()}")
    Ok(_)  -> panic("unreachable")
  }
}
-- data is in scope here
```

Two problems:

1. The `Ok(_) -> panic("unreachable")` arm is pure ceremony. The
   `when let Ok(data) = res else` branch is only entered when `res`
   is NOT `Ok(_)`, yet match exhaustiveness forces the arm anyway.
2. If the user ignores the ceremony and writes `Err(e) -> ...` alone,
   the compiler rejects the match as non-exhaustive even though at
   that program point `Ok(_)` is provably unreachable.

The surface-level want: "inside the else, let me match on the
remaining variants without re-stating the one already handled."

## Non-goals

- **No type narrowing.** `res`'s type does not change inside the
  else block. Flow-sensitive narrowing is an explicit non-goal;
  see the alternatives below for why.
- **No implicit variable bindings.** Every binder is introduced by
  a pattern the user wrote.
- **No changes to `?`, `match`, or regular `when let`** without an
  `else match` continuation. Existing code compiles unchanged.

## Design

New syntactic form:

```silt
when let <pattern> = <scrutinee> else match <same-scrutinee> {
  <arms>
}
```

Semantics:

1. Evaluate the scrutinee.
2. If it matches `<pattern>`, bind the pattern's variables in the
   surrounding scope after the `when let` and fall through.
3. Otherwise, match the scrutinee against `<arms>` as a regular
   `match`, with the primary pattern implicitly included as already-
   covered for exhaustiveness purposes. Arms in the else-match
   branch must diverge (`return`, `panic`, etc.) — same rule as
   today's `when let ... else { ... }` body.

Exhaustiveness rule: the union of `<pattern>` and `<arms>` must cover
the scrutinee's type. Writing the same pattern in both positions is
a warning (dead arm).

### Example: Result

```silt
when let Ok(data) = load() else match load_result {
  Err(IoNotFound(path)) -> create_default(path)
  Err(other) -> panic("load failed: {other.message()}")
}
```

`data: LoadedData` is in scope after the block. The else-match
covers `Err(_)` fully; no `Ok(_) -> ...` arm required.

### Example: Option

```silt
when let Some(v) = option_value else match option_value {
  None -> return Err("missing required value")
}
```

### Example: three-variant enum

```silt
type State { Idle, Loading, Error(String) }

when let Idle = state else match state {
  Loading -> wait_and_retry()
  Error(msg) -> return Err(msg)
}
```

### Short form for single-arm else

Common case: the else branch has a single arm. Sugar for that:

```silt
when let <pattern> = <scrutinee> else <arm-pattern> -> <arm-body>
```

Desugar:

```silt
when let <pattern> = <scrutinee> else match <scrutinee> {
  <arm-pattern> -> <arm-body>
}
```

Example:

```silt
when let Ok(data) = res else Err(e) -> panic("load failed: {e.message()}")
```

Same exhaustiveness rule: `<pattern>` + `<arm-pattern>` must cover
the scrutinee's type. For `Result(T, E)` with `Ok(data)` as the
primary, `Err(e)` is the complement — fully covers the type in one
arm.

The short form is purely syntactic sugar; the typechecker expands
it to the long form and processes it identically.

## Alternatives considered

### Option A — keep `else { ... }`, require explicit `match`

Status quo. Every user that needs variant-aware handling in the
else re-states an unreachable primary arm. Rejected: the ceremony
is silly and the `unreachable` arm is a known bad code smell.

### Option B — automatic type narrowing of the scrutinee inside else

```silt
when let Ok(data) = res else {
  panic("failed: {res.message()}")  -- res is AppError here
}
```

Rejected, for two reasons:

1. **Philosophy.** Silt's types are nominal and non-surprising:
   a variable has one type across its lexical scope. Introducing
   flow-sensitive narrowing is a TypeScript / Kotlin idiom that
   diverges from the Elm / Gleam / Rust camp silt aligns with.
2. **Scope.** Narrowing `Result(a, e)` to `e` requires the type
   system to special-case Result / Option, or to introduce variant-
   subtraction types (`Result(a, e) \ Ok(a) = e`) as a general
   feature. Either is a large change whose blast radius goes
   beyond this feature's motivating case. A bounded `else match`
   addresses the same user need with none of the type-system cost.

### Option C — `else <pattern> { ... }` single-complement binding

```silt
when let Ok(data) = res else Err(e) {
  panic("failed: {e.message()}")
}
```

A one-pattern complement binder. Rejected as the primary design
because it doesn't scale past 2-variant enums: for `State { Idle,
Loading, Error(_) }`, `when let Idle = s else <single-pattern>`
can't cover both `Loading` and `Error(_)` in one production.

The **short form** of the proposed design subsumes this case — it's
syntactically identical for the 2-variant scenario — so users get
Option C's ergonomics for Result / Option without losing
generality for larger enums.

### Option D — `guard`-style block only (no pattern refinement)

Leave the else branch unchanged but introduce a `guard` form that
mirrors Swift's `guard let`. No improvement over status quo:
the unreachable arm problem persists inside the else block.

Rejected: solves nothing the `else { ... }` form doesn't already.

### Option E — implicitly-bound complement variable

```silt
when let Ok(data) = res else {
  -- synthesized binding: `__else__ = the Err variant`
  panic(__else__.message())
}
```

Rejected: implicit bindings are not silt's style. User writes what
they want to reference.

## Language survey

How similar constructs in other languages handle the else branch:

- **Rust** `let Some(x) = opt else { return; };` — else must
  diverge. No narrowing, no implicit complement. User writes
  `match` inside the else if they need variant-aware handling.
- **Swift** `guard let x = opt else { return }` — same. Swift
  does have `if case let` but does not narrow.
- **OCaml / Gleam / Elm / Haskell** — no `when let` equivalent;
  users write `match` / `case of` with all variants explicitly.
  Exhaustiveness checking is non-negotiable.
- **Kotlin** smart-casts narrow types across conditional branches,
  relying on the type system's understanding of discriminated
  unions. Not applicable to silt's nominal enum model without a
  substantial type-system change.
- **TypeScript** flow-sensitive narrowing. Same objection.

No mainstream explicit-error language offers the narrowing Option B
proposes. The `else match` form in this proposal is a silt-specific
extension whose closest analogue is "writing a match in the else
manually, but with exhaustiveness that already knows the primary
pattern was handled."

## Interaction with existing features

### `?` operator

Unaffected. `?` stays as pure propagation. `when let ... else match`
is for cases where the user wants divergent handling with richer
control than `?` provides.

### `return` / `panic`

Arms in the else-match block must diverge. Same rule as today's
`else { ... }` body. Enforced by the typechecker the same way:
the branch's type must be `Never`.

### Guards in arms

Arms in the else-match may use `when` guards as in regular match:

```silt
when let Ok(n) = parse(s) else match parse(s) {
  Err(e) when e == ParseEmpty -> return Err("empty input")
  Err(other) -> return Err("bad: {other.message()}")
}
```

Same semantics as regular match guards.

### Shadowing

The else-match's arm patterns may introduce bindings that shadow
enclosing names. Same rules as `match`: arm bindings are scoped to
the arm body.

## Implementation notes

### Parser

One new production in `parse_when_let`:

```
WhenLetStmt := "when" "let" Pattern "=" Expr ("else" ElseBranch)?
ElseBranch  := Block
             | "match" Expr "{" MatchArm+ "}"
             | Pattern "->" Expr
```

The short form (`Pattern "->" Expr`) is desugared at parse time to
the long form.

Estimated diff size: 50-100 lines in `src/parser.rs`.

### Typechecker

The else-match's exhaustiveness uses the existing variant-coverage
pass with one extension: pre-populate the covered-variant set with
the variant(s) reachable through the primary `when let` pattern
before checking arms.

For `when let Ok(data) = res else match res { ... }`:

1. Compute the variants `Ok(data)` covers: `{Ok}`.
2. Run the existing match-exhaustiveness pass against the else's
   arms, seeded with `{Ok}` as already-covered.
3. At the end, required-coverage set must be empty.

Dead-arm detection: if any arm of the else-match overlaps with
the primary pattern, warn. (Writing `Err(e) -> ...` AND `Ok(_) -> ...`
in the else is reachable only through the second arm; the first
covers the primary's complement.)

Estimated diff size: 50-100 lines in `src/typechecker/` and the
pattern-coverage helpers.

### Codegen

Lower `when let P = E else match E { arms }` to:

```
match E {
  P -> bind P's variables and fall through
  <arms> -> evaluate arm body (which must diverge)
}
```

This is a regular `match` with one extra synthesized arm — no new
opcodes. Estimated diff size: < 50 lines in the compiler.

### Tests

- Cover Result, Option, 2-variant user enum, 3+ variant user enum.
- Exhaustiveness errors (missing variants in else-match).
- Dead-arm warning (overlapping with primary pattern).
- Nested `when let ... else match` inside match arms inside when-let.
- Short form (`else P -> body`).
- Interaction with `?` in the else arms.
- Interaction with guards (`when` clauses) in else arms.

Estimated test count: 15-20. File: `tests/when_let_else_match_tests.rs`.

## Cost estimate

- Parser: ~0.25 day
- Typechecker exhaustiveness extension: ~0.5 day
- Codegen: trivial once exhaustiveness is settled
- Tests: ~0.5 day
- Documentation in `docs/language/pattern-matching.md` and
  `docs/language/error-handling.md`: ~0.25 day

**Total: ~1.5 days.** Scope is bounded; no type-system rework.

## When to implement

Not blocking. The status-quo `else { match res { ... } }` with an
`Ok(_) -> panic("unreachable")` arm works, just ugly. Schedule
when:

1. A real silt program hits the pattern 3+ times and the user asks.
2. Slack appears after the current round of stdlib / formatter
   work settles.

Track as a single follow-up task.

## Open questions

**Should the else-match's scrutinee be required to match the
when-let's scrutinee?** Two options:

- **Strict:** `when let P = X else match X { ... }` — the `X`s must
  be the same identifier. Enforces the "this is the complement of
  the primary match" interpretation and lets the compiler use
  parse-level reasoning for exhaustiveness pre-population.
- **Relaxed:** allow any expression after `else match`. More
  flexible but the exhaustiveness shortcut is lost when the two
  scrutinees differ.

Recommend **strict** as the v1 rule. It matches the motivating use
case and keeps the feature narrowly scoped. Relaxing later is easy;
tightening later would break users.

**Should the short form allow bindings in the arm pattern but not
guards?** Keeping guards out of the short form keeps it terse.
Users who need guards write the long form. Recommend: short form
accepts arm patterns with bindings (`Err(e) -> ...`) but not `when`
guards. Long form supports guards as in any other match.

## Rejected alternatives (brief)

| Approach | Why not |
|----------|---------|
| Type narrowing of scrutinee in else block | Flow-sensitive type system, non-goal. |
| Implicit complement variable in else | Magic binding, off-brand for silt. |
| `else <pattern> { ... }` single-pattern only | Doesn't generalize past 2 variants. |
| `guard` keyword parallel to `when let` | Adds a keyword without solving the exhaustiveness ceremony. |
| Allow any divergent expr in else, skip exhaustiveness | Loses the main benefit — verified handling of the complement. |

## Related work

- [`docs/language/pattern-matching.md`](../language/pattern-matching.md) —
  existing match semantics, the exhaustiveness model this proposal
  extends.
- [`docs/language/error-handling.md`](../language/error-handling.md) —
  the `when let ... else` form today, which this proposal supersedes
  in the error-handling context (the status-quo form remains valid
  for non-pattern-divergent else branches).
