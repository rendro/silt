---
title: "Proposal: `From` trait for error conversion"
section: "Proposals"
status: draft
---

# `From` trait for error conversion

**Status:** proposal, not yet implemented.
**Scope:** opt-in user-declared conversion between error enums, to
reduce `result.map_err(r, { e -> Wrap(e) })` boilerplate in
cross-module composition.

## Problem

With the typed stdlib errors landed (see
[`stdlib-errors.md`](stdlib-errors.md), Phases 0-3), any silt function
that spans multiple stdlib modules has to wrap each module's error
into a local `AppError` enum and pipe every call through
`result.map_err`:

```silt
fn load_config(path: String) -> Result(Config, AppError) {
  let raw    = result.map_err(io.read_file(path),    { e -> IoProblem(e) })?
  let config = result.map_err(json.parse(raw, Config), { e -> JsonProblem(e) })?
  Ok(config)
}
```

For functions that span 3-4 modules this is real boilerplate — the
wrapper tag is the only non-mechanical bit on every line, and it's
easy to pick the wrong variant (we've already seen tests mismatch
`ConfigParse` vs `ApiResponse` in a refactor because the line shape is
so repetitive).

## Non-goals

- **No implicit coercion on `?`.** The proposal explicitly from-trait
  decisions when error types mismatch ruled out (see
  `stdlib-errors.md` Decisions section): `?` is propagation, never
  conversion. This avoids Rust's `From`+`?` combination where `?`
  silently performs a typed rewrite.
- **No trait objects.** Silt has no existentials, so a trait-object
  `dyn Error` escape hatch is not on the table.
- **No auto-derive.** Each conversion is user-declared. The compiler
  never invents an `impl From for AppError` without the user writing
  it.

## Options considered

### Option A — keep `result.map_err`

Status quo. Pros: explicit, already works, one idiom. Cons: real
line-noise in multi-module fns; hand-wrapping is mechanical and
easy to mis-tag.

### Option B — `.into()` via a `From` trait (recommended)

A user-declared `trait From` (same name as Rust's) that each
conversion target implements per source:

```silt
trait From(Source) {
  fn from(source: Source) -> Self
}

type AppError {
  IoProblem(IoError),
  JsonProblem(JsonError),
}

impl From(IoError) for AppError {
  fn from(e: IoError) -> AppError { IoProblem(e) }
}

impl From(JsonError) for AppError {
  fn from(e: JsonError) -> AppError { JsonProblem(e) }
}
```

Call-site:

```silt
fn load_config(path: String) -> Result(Config, AppError) {
  let raw    = io.read_file(path).into()?
  let config = json.parse(raw, Config).into()?
  Ok(config)
}
```

`.into()` is a user-level method added automatically when `From(X) for Y`
is implemented. It lifts `Result(a, X)` to `Result(a, Y)` by applying
`Y::from` to the Err, passing Ok through unchanged. `?` then
propagates the lifted Result.

The conversion is explicit (the user wrote `.into()`), the mapping is
visible at the trait impl site, and there's no "`?` silently
converts" ambiguity.

### Option C — overloaded `?` operator

A `?` whose behavior includes automatic `From`-conversion (Rust's
current design). Rejected in the existing decisions section: "No
auto-wrap on `?`. Explicit-over-implicit."

### Option D — a compiler-inserted coercion

A whole-program analysis that inserts `Wrap(e)` where the source and
target error types differ by one enum variant. Too magical, too
fragile to refactors; no library author will predict what the
compiler inferred.

## Recommendation

**Option B.** The explicit `.into()` call is the minimum ergonomic
improvement over `map_err` that doesn't compromise the "explicit over
implicit" stance. It reads naturally:

- `io.read_file(path).into()?` — "lift this into my error type, then
  propagate."
- The conversion from `IoError` to `AppError` is visible in the
  `impl From(IoError) for AppError` block.
- No new keywords. No magic on `?`. No trait objects.

## Semantics sketch

`trait From(T)` has one method: `fn from(value: T) -> Self`.

When `impl From(SrcErr) for DstErr` is declared, the compiler
synthesizes two inherent methods on `Result(_, SrcErr)`:

- `.into_err(): Result(a, DstErr)` — explicit, unambiguous.
- `.into(): Result(a, DstErr)` — sugar, resolves via type inference
  when a single target type fits.

The `.into()` form dispatches by **target-type inference** only, so
it works when:
- A `let` binding has an explicit `Result(_, DstErr)` annotation.
- The value is being returned from a function whose return type is
  `Result(_, DstErr)`.
- The value is passed to a parameter whose type is `Result(_, DstErr)`.

If type inference finds zero or multiple matching `From` impls, the
compiler emits a hard error asking the user to use `.into_err()` with
an explicit type annotation.

## Interaction with `?`

`?` keeps its current semantics: propagate the Err unchanged if the
caller's Err type matches, otherwise type-check fails. Adding `From`
does not weaken `?`. The user writes `.into()?` to chain conversion
and propagation explicitly:

```silt
let raw = io.read_file(path).into()?  -- convert IoError -> AppError, then ?
```

The separation keeps the compiler behavior locally predictable: `?`
never changes types.

## Extension: `From(T)` as an auto-derive target

If the `derive` syntax for silt traits lands (separate proposal),
`From` impls where every arm is a single-variant wrap could be
auto-derived:

```silt
#[derive(Error, From)]
type AppError {
  IoProblem(IoError),
  JsonProblem(JsonError),
  Custom(String),   -- no single-source From; skipped
}
```

This would reduce the `load_config` example to zero manual impls.
Out of scope for v1 of this proposal — gated on the existence of a
more general `#[derive]` machinery.

## Cost estimate

- Trait declaration + parameterized-trait plumbing: 1 day
  (silt already has some support for type-parameterized traits — see
  `docs/language/generics.md`; confirm parameterized **trait**
  declarations also work, which they may not yet).
- `.into()` / `.into_err()` method synthesis on Result: 0.5 days.
- Type-inference hook for target-type resolution on `.into()`: 1 day.
- Tests + documentation: 0.5 days.

Total: approximately three days.

## When to implement

Not blocking. Users currently have `result.map_err` and it works.
Implement when:
- Parameterized traits are confirmed working (may require language
  work first).
- A real program hits ~5+ `map_err` calls in a single function and
  the reduction is measurable.

Track via a single follow-up task.
