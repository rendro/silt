---
title: "Proposal: `From` trait for error conversion"
section: "Proposals"
status: deferred
---

# `From` trait for error conversion

**Status:** deferred. Design direction validated, but blocked on silt
compiler work whose scope exceeds what the ergonomic win justifies
today.

**Scope (aspirational):** opt-in user-declared conversion between
error enums so call sites reference a single named conversion
function instead of repeating inline closures. **No new sugar on
`?`, no `.into()` auto-conversion.** Traits declare methods; they do
not perform magic coercion.

## Status summary

The design described here matches silt's principles and the wider
explicit-error community's consensus — see the Language Survey
below. But when evaluated against silt's current compiler, two
load-bearing assumptions are false:

1. **Multiple `From(Src) for One` impls cannot be disambiguated.**
   `src/compiler/mod.rs:793` registers every impl method as
   `TargetType.method_name` with no trait argument in the qualified
   name. Two impls — `From(IoError) for AppError` and
   `From(JsonError) for AppError` — both register as `AppError.from`,
   and the second overwrites the first. Reproduced end-to-end as of
   the state at the time this proposal was evaluated.

2. **Argument-type-directed trait resolution for static methods is
   not a silt feature today.** Silt's static trait methods dispatch
   off `Self` (the impl target), not off the argument's type. To
   pick between `From(IoError) for AppError` and `From(JsonError)
   for AppError`, the compiler would need trait-argument-directed
   dispatch for static methods, plus let-binding / call-context
   inference to resolve which impl fires when `AppError.from` is
   used as a first-class value.

Together these make the proposal a multi-week compiler change, not a
drop-in design. Defer until either the cost is justified by real
observed pain, or the dispatch machinery lands for independent
reasons.

Workarounds that are **implementable today without any compiler
work** are described below — the "Available today" section is the
practical guide for users who hit this now.

## Problem

With the typed stdlib errors landed, any silt function that spans
multiple stdlib modules has to wrap each module's error into a local
`AppError` enum and lift every call through `result.map_err` with a
closure:

```silt
fn load_config(path: String) -> Result(Config, AppError) {
  let raw    = result.map_err(io.read_file(path),       { e -> IoProblem(e) })?
  let config = result.map_err(json.parse(raw, Config),  { e -> JsonProblem(e) })?
  Ok(config)
}
```

For functions that span 3-4 modules this is real boilerplate — the
wrapper tag is the only non-mechanical bit on each line, and the
repeated `{ e -> Wrap(e) }` shape is easy to mis-tag in a refactor
(already seen in practice: `ConfigParse` vs `ApiResponse` swaps in a
pipeline of near-identical lines).

## Design principles (non-negotiable)

- **`?` stays pure propagation.** It never performs a type
  conversion. Matches Gleam / Elm / F# / Haskell / Zig — the entire
  explicit-error camp. Rejects Rust's `?` + `From` combination as the
  one design choice in that camp that users consistently complain
  about.
- **No `.into()` sugar.** A trait declares methods on a type. It
  does not define "magic coercion" that the compiler inserts at
  call sites or that resolves via target-type inference. The
  conversion function name must be visible at the call site.
- **No auto-derive.** Each `From(Src)` impl is user-declared. The
  compiler never invents an impl that wasn't written.
- **No trait objects / existentials.** Silt has none; this proposal
  doesn't introduce them.

## Design (aspirational)

### The trait

```silt
trait From(SourceType) {
  fn from(source: SourceType) -> Self
}
```

`From` takes a type parameter for the source type. Each impl pins
one source to one `Self`. The method `from` converts a value of the
source type into a `Self`. There is no default body; every impl
provides one.

### Example

```silt
type AppError {
  IoProblem(IoError),
  JsonProblem(JsonError),
  Custom(String),
}

impl From(IoError) for AppError {
  fn from(e: IoError) -> AppError { IoProblem(e) }
}

impl From(JsonError) for AppError {
  fn from(e: JsonError) -> AppError { JsonProblem(e) }
}
```

### Call sites (once the compiler supports this)

```silt
fn load_config(path: String) -> Result(Config, AppError) {
  let raw    = result.map_err(io.read_file(path),      AppError.from)?
  let config = result.map_err(json.parse(raw, Config), AppError.from)?
  Ok(config)
}
```

Compared to the pre-proposal shape, `{ e -> IoProblem(e) }` becomes
`AppError.from`. The conversion is named; readers follow the trait
impl to see what it does.

### Honest note on "how magic is this?"

Calling out the residual implicit-ness that an earlier draft of this
proposal glossed over:

Resolving `AppError.from` at a call site like
`result.map_err(io.read_file(path), AppError.from)` requires the
typechecker to:

1. See `map_err`'s second arg expects `Fn(IoError) -> f`.
2. Propagate that constraint to the resolution of `AppError.from`.
3. Pick the `From(IoError) for AppError` impl.

Steps 2-3 are **type-directed resolution** — the same mechanism
that makes Rust's `.into()` feel like magic. The win over `.into()`
is that the **constructor name is visible at the call site**, not
that the dispatch mechanism is fundamentally more explicit. Be
honest about what we're buying:

- ✅ Grep-ability of the conversion (search for `AppError.from`).
- ✅ One place to document each conversion (the impl block).
- ✅ No `.into()` method synthesis on every type.
- ❌ Still requires the reader to understand that `AppError.from`
  resolves differently depending on call context.
- ❌ Still needs the compiler to do argument-type-directed dispatch
  to pick the right impl.

## Language survey (why this shape)

Every statically-typed explicit-error language the silt community
looked at rejected Rust's `.into()` + `?` approach when they
considered it:

- **Gleam**: unified error type proposals closed in favor of keeping
  `result.map_error` explicit. A unified error exists only as a
  library (`snag`) that adds context strings, not as a compiler
  feature.
- **OCaml**: `Result.map_error` is canonical. The more sophisticated
  camp uses polymorphic variants so error types merge structurally
  — no conversion ceremony, but also no conversion trait.
- **Haskell**: `withExceptT :: (e -> e') -> ExceptT e m a -> ExceptT e' m a`.
  The conversion is an explicit first-class function. No typeclass
  resolves it implicitly despite Haskell having every piece of
  machinery to do so.
- **F#**: `|> Result.mapError f`. Boilerplate reduction comes from
  computation expressions (`result { ... }`), which declare the
  wrap at the block level — not from trait resolution.
- **Elm**: `Result.mapError` + hand-written wrapper types. The
  community explicitly rejected anything more magic.
- **Zig**: error sets unify structurally at the type level. No
  user-defined conversion.

Common thread: ergonomic wins come from structural unification or
call-site pipeline sugar, not from type-directed resolution. This
proposal partially honors that (wrap stays named at the call site)
while still requiring inference under the hood. Worth knowing which
bits are the real wins.

## Available today (no compiler work)

Users who hit `result.map_err` boilerplate *now* have two options
that work on the current silt tree:

### 1. Keep `result.map_err` with closures

```silt
fn load_config(path: String) -> Result(Config, AppError) {
  let raw    = result.map_err(io.read_file(path),      { e -> IoProblem(e) })?
  let config = result.map_err(json.parse(raw, Config), { e -> JsonProblem(e) })?
  Ok(config)
}
```

Three extra characters per line (`{e->`). Works. Matches Gleam.

### 2. One trait per target AppError type

```silt
trait IntoAppError {
  fn into_app_error(self) -> AppError
}

impl IntoAppError for IoError {
  fn into_app_error(self) -> AppError { IoProblem(self) }
}

impl IntoAppError for JsonError {
  fn into_app_error(self) -> AppError { JsonProblem(self) }
}

fn load_config(path: String) -> Result(Config, AppError) {
  let raw    = result.map_err(io.read_file(path),      IoError.into_app_error)?
  -- OR using a closure to make the conversion name visible on the value side:
  let cfg    = result.map_err(json.parse(raw, Config), { e -> e.into_app_error() })?
  Ok(cfg)
}
```

This **works in silt today** because dispatch is by `Self` (each
source type has one impl, no collision). Drawbacks:

- One `trait IntoX` per target AppError type — doesn't reuse.
- First-class method reference form (`IoError.into_app_error`) hits
  the same limits as `AppError.from` when passed as a value, so the
  closure form may still be needed at some call sites.

Still ugly. But it's the best pattern available without compiler
work.

## Compiler work the aspirational design needs

For the `trait From(Src)` design to actually ship, the silt compiler
needs:

1. **Trait-param-aware qualified names.**
   `src/compiler/mod.rs:793` must include the trait's argument in
   the global key, or impls must be stored in a separate table keyed
   by `(target_type, trait_name, trait_args, method_name)`. Current
   single-flat-namespace registration blocks multi-impl.

2. **Argument-type-directed dispatch for static trait methods.**
   When the typechecker sees `AppError.from(e)` where
   `e: IoError`, it must pick the `From(IoError) for AppError` impl
   based on `e`'s type. This is new machinery: existing dispatch
   picks by Self only.

3. **Inference flow when `AppError.from` is a value.**
   `let f = AppError.from` with no immediate call can't pick an
   impl — either require an ascription (`let f: Fn(IoError) -> AppError = AppError.from`)
   and teach the typechecker to use it, or defer monomorphization
   to first use. Both are new capabilities.

4. **Error messages.** Ambiguous `AppError.from` (e.g. passed to a
   polymorphic helper with no disambiguation) needs a clear
   diagnostic pointing at all the `From(X) for AppError` impls in
   scope.

5. **Test coverage for overlap edge cases.** What happens with
   `From(a) for AppError` (a blanket impl) plus a specific
   `From(IoError) for AppError`? Coherence rules, specialization
   behavior, or a rejection. Silt's existing trait system doesn't
   currently answer this.

Realistic budget: **1-2 weeks of typechecker/compiler work**, and
an unknown tail risk of adjacent generic-trait regressions.

## Rejected alternatives

| Approach | Why rejected |
|----------|--------------|
| `.into()` method with target-type inference | The magic this proposal explicitly rejects — and even with a visible name the mechanism is near-identical. |
| `?` auto-conversion (Rust-style) | Compounds the above with a second inference layer. Explicitly ruled out in principles. |
| Compiler-inserted coercion on assignment | Whole-program magic; refactors produce surprising conversions. |
| `result.wrap_err(r, Constructor)` as the only mechanism | Fine as a helper, but doesn't centralize conversions — same variant may be passed at 20 call sites. |
| Unified `SiltError` stdlib type | Loses the module-specific pattern-match granularity the typed errors just shipped. |
| Polymorphic variants (OCaml-style) | Requires a structural-typing extension to silt's nominal type system. Out of scope. |
| **One trait per source type** (`trait IntoAppError { fn into_app_error(self) -> AppError }`) | Implementable today. Rejected as the *proposal* design because it doesn't scale — one new trait per target AppError. But it's the right **available-today workaround** and is documented above. |
| **Inverted `trait IntoErr(target) for Source`** | Dispatches by Self. Works today for multi-source → one-target IF silt supports trait-param inference from target-type ascription (unverified). Rejected because call site becomes `io_err.into_err()` with target inferred from context — i.e. Rust's `.into()` with a different name, the very thing the proposal exists to reject. |

## Non-goals clarified

### Why not `.into()`?

Covered above. The trait-based design still requires type-directed
resolution under the hood, but keeps the conversion function name
visible at the call site. That visibility is the improvement over
`.into()` — not a difference in dispatch mechanism.

### Why not `?` auto-conversion?

Same reason, amplified. `?`-auto-convert requires *two* inference
passes: one to know where the `?` is returning to, one to resolve
the `From`. Two layers of compiler magic on one operator. Multiple
surveyed languages explicitly rejected this (Gleam, Elm, Zig) or
never considered it (OCaml, Haskell, F#). Only Rust adopted it, and
even there it is the single most common complaint about error
handling readability. Not a road worth taking.

### Why not auto-derive?

Two reasons:
1. Silt has no general `#[derive]` machinery for user-declared
   traits. Auto-derive for `From` alone would be a bespoke compiler
   feature that we'd have to justify on its own.
2. Even with a general derive system, auto-derived `From` impls
   hide the wrap variant from the impl site. If `AppError` has
   `IoProblem(IoError)`, a derived `impl From(IoError) for AppError`
   obviously maps to `IoProblem(e)` — but the user has to know
   "derived From picks the single-variant wrap" to verify the
   mapping is what they want. Explicit impls keep the mapping on
   the page.

If `#[derive]` lands and proves itself for less-ambiguous traits
(Display, Equal, etc.), auto-derived `From` becomes a natural
follow-up. Not part of v1.

## When to unblock this proposal

Pause. Do not start compiler work until one of these triggers fires:

1. **A real silt program hits the pain.** If after six months of
   Phase 2+3 stdlib errors in the wild, cross-module composition
   complaints are the top ask, the cost becomes worth it.
2. **The compiler grows trait-param-aware dispatch for other
   reasons.** If a non-error feature (e.g. a proper `Serializer(T)`
   trait with multiple impls) forces the same compiler work, `From`
   rides on it.
3. **The "one trait per target" workaround proves untenable.** If
   teams end up with so many `IntoXError` traits that the workaround
   itself becomes the boilerplate, that's signal to build the real
   thing.

Until then, users write `result.map_err(r, { e -> Wrap(e) })` or
the per-target-trait pattern above.

## Related work

- `super::docs::ERRORS_MD` (in `src/typechecker/builtins/docs.rs`) —
  the module-local typed-error system this proposal layers on top of.
  Round 62 phase-2 inlined the former `docs/stdlib/errors.md` into
  the typechecker so it surfaces through LSP hover.
- [`examples/cross_module_errors.silt`](../../examples/cross_module_errors.silt) —
  canonical example of the current `result.map_err`-based idiom.
  Unchanged by this proposal's deferred status.
