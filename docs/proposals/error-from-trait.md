---
title: "Proposal: `From` trait for error conversion"
section: "Proposals"
status: draft
---

# `From` trait for error conversion

**Status:** proposal, not yet implemented.
**Scope:** opt-in user-declared conversion between error enums. The
goal is to centralize cross-module error conversions in a trait
impl so call sites reference a single named conversion function
instead of repeating inline closures. **No new sugar on `?`, no
`.into()` auto-conversion.** Traits declare methods; they do not
perform magic coercion.

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
  conversion function must be visible at the call site.
- **No auto-derive.** Each `From(Src)` impl is user-declared. The
  compiler never invents an impl that wasn't written.
- **No trait objects / existentials.** Silt has none; this proposal
  doesn't introduce them.

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
- **Zig**: error sets unify structurally at the type level. No user-
  defined conversion.

Common thread: **ergonomic wins come from structural unification or
call-site pipeline sugar, not from type-directed resolution.** This
proposal honors that — the wrap stays named at the call site.

## Design

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

### Call sites

The trait method is referenced as a first-class value. Because
`AppError.from` is a named function of type `SourceType -> AppError`,
it passes directly to `result.map_err` without a closure wrapper:

```silt
fn load_config(path: String) -> Result(Config, AppError) {
  let raw    = result.map_err(io.read_file(path),      AppError.from)?
  let config = result.map_err(json.parse(raw, Config), AppError.from)?
  Ok(config)
}
```

Compared to the pre-proposal shape:
- `{ e -> IoProblem(e) }` → `AppError.from`
- The conversion is named; readers see exactly which function is
  converting. They follow the trait impl to see what it does —
  standard method lookup, same as any other method call.
- The wrap variant (`IoProblem` vs `JsonProblem`) is resolved by
  trait-method dispatch on the *argument type* of `AppError.from`,
  i.e. which `impl From(X) for AppError` matches `X = IoError`.
  This is ordinary method dispatch, not a special conversion pass.

### What the compiler resolves, and how

The only compiler work specific to `From` is allowing
`AppError.from` to be referenced as a first-class function whose
concrete instantiation is picked by the *argument type* of the
call that consumes it.

Concretely, when the typechecker sees:

```silt
result.map_err(io.read_file(path), AppError.from)
```

- `io.read_file(path)` has type `Result(String, IoError)`.
- `result.map_err`'s second argument expects `(IoError) -> ?`.
- `AppError.from` is a trait method keyed by `From(SourceType) for AppError`.
- The compiler unifies `SourceType = IoError`, looks up
  `impl From(IoError) for AppError`, and resolves the concrete
  `from` body.

This is **argument-type-directed resolution**, which silt already
has to do for any generic trait method passed as a value. It is
distinct from Rust's `.into()`, which does **return-type-directed
resolution** — the thing that produces the "I have no idea which
From impl fired" readability complaint.

If the caller passes `AppError.from` somewhere the argument type is
ambiguous (e.g. inside a generic helper with no fixed source type),
the compiler emits a hard error asking for an explicit type
ascription: `AppError.from::<IoError>` or equivalent.

### Explicit override when resolution won't fit

If at some point a caller needs the conversion and argument-type
dispatch isn't enough (typically because silt's method-as-value
feature has a limitation we haven't closed), the user can always
fall back to an eta-abstracted closure:

```silt
let raw = result.map_err(io.read_file(path), { e -> AppError.from(e) })?
```

Same behavior, one extra `{ e -> ... }` hop. The `AppError.from`
identifier stays visible.

## Interaction with `?`

`?` keeps its current semantics exactly: propagate the Err unchanged
if the caller's Err type matches, otherwise type-check fails. `From`
impls do not hook into `?`. Any conversion is a separate explicit
call:

```silt
let raw = result.map_err(io.read_file(path), AppError.from)?
--                      ^^^^^^^^^^^^^^^^^^^  ^^^^^^^^^^^^  ^
--                      what to convert      converter     then propagate
```

## Non-goals clarified

### Why not `.into()`?

`.into()` in Rust is a method that the compiler synthesizes on every
type that has a `From` impl, and its behavior depends on the target
type inferred from the surrounding context. That is:
- The method has no declared body.
- Its meaning changes based on the expected return type at the call
  site.
- The caller can't tell which `From` impl fired without checking
  every `From for X` where X might be the inferred target.

Silt's trait system declares methods that do something defined,
bound to specific types. `.into()` would be the first silt method
whose meaning is defined by inference, not by the trait impl that
named it. That's a category difference — and it's the exact
property this proposal exists to reject.

A future `.into()` sugar can be added non-breakingly on top of this
design if we ever want it. Start without.

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
   traits. Auto-derive for `From` alone would be a bespoke
   compiler feature that we'd have to justify on its own.
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

## Cost estimate

- Parameterized trait declarations (`trait From(T) { ... }`) — unclear
  whether silt already supports this. Needs confirmation. If not,
  this proposal is blocked on a language change that's probably
  scoped broadly enough to be its own proposal (type-parameterized
  traits generally, not just for From).
- First-class trait-method-as-value lookup for `AppError.from`. Silt
  supports methods on concrete types; needs verification that
  `TypeName.method_name` produces a callable value when
  `method_name` is resolved via a trait impl.
- Tests + documentation: ~0.5 days once the above are in place.

Budget: **one day if both primitives exist; several days if the
language work has to happen first.** Worth confirming the two
primitives before committing to a delivery date.

## When to implement

Not blocking. Users have `result.map_err` with closures and it works.
Implement when:
- Parameterized traits are confirmed working (or their absence is
  confirmed and we accept the language work as the gating item).
- A real silt program is observed with 4+ `result.map_err(r, { e ->
  Wrap(e) })` lines in a single function and a measurable net
  reduction in ceremony from this design.

Track via a single follow-up task.

## Rejected alternatives

| Approach | Why rejected |
|----------|--------------|
| `.into()` method with target-type inference | The magic this proposal explicitly rejects. |
| `?` auto-conversion (Rust-style) | Compounds the above with a second inference layer. |
| Compiler-inserted coercion on assignment | Whole-program magic; refactors produce surprising conversions. |
| `result.wrap_err(r, Constructor)` as the only mechanism | Fine as a helper, but doesn't centralize conversions — same variant may be passed at 20 call sites. `From` impl + `AppError.from` gives one canonical name. |
| Unified `SiltError` stdlib type | Loses the module-specific pattern-match granularity the typed errors just shipped. |
| Polymorphic variants (OCaml-style) | Requires a structural-typing extension to silt's nominal type system. Out of scope. |

## Related work

- [`docs/stdlib/errors.md`](../stdlib/errors.md) — the module-local
  typed-error system this proposal layers on top of.
- [`examples/cross_module_errors.silt`](../../examples/cross_module_errors.silt) —
  canonical example of the current `result.map_err`-based idiom.
  After this proposal lands, that example updates to use
  `AppError.from` and shrinks to one meaningful line per conversion.
