---
title: "Proposal: `From` trait for error conversion"
section: "Proposals"
status: accepted
---

# `From` trait for error conversion

**Status:** accepted, and built: a trait with a parameter has an impl
for a type at each trait argument, and the argument a use decides
selects it ([Traits — Parameterized Traits](../language/traits.md#parameterized-traits)).
A program declares `From` and its impls as below; silt has no `From` of
its own.

**Scope:** opt-in user-declared conversion between error enums so call
sites reference a single named conversion function instead of
repeating inline closures. **No new sugar on `?`, no `.into()`
auto-conversion.** Traits declare methods; they do not perform magic
coercion.

## Problem

With the typed stdlib errors landed, any silt function that spans
multiple stdlib modules has to wrap each module's error into a local
`AppError` enum and lift every call through `result.map_err` with a
closure:

```silt
fn load_config(path: String) -> Result(Config, AppError) {
  let raw = result.map_err(io.read_file(path)) { e -> IoProblem(e) }?
  let config = result.map_err(json.parse(raw, Config)) { e -> JsonProblem(e) }?
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
- **No impl made by the compiler.** Each `From(Src)` impl is user-declared. The
  compiler never invents an impl that wasn't written.
- **No trait objects / existentials.** Silt has none; this proposal
  doesn't introduce them.

## Design

### The trait

```silt
trait From(e) {
  fn from(source: e) -> Self
}
```

`From` takes a type parameter for the source type. Each impl pins
one source to one `Self`. The method `from` converts a value of the
source type into a `Self`. There is no default body; every impl
provides one.

### Example

```silt
type AppError {
  IoProblem(io.IoError),
  JsonProblem(json.JsonError),
  Custom(String),
}

trait From(io.IoError) for AppError {
  fn from(e: io.IoError) -> AppError {
    IoProblem(e)
  }
}

trait From(json.JsonError) for AppError {
  fn from(e: json.JsonError) -> AppError {
    JsonProblem(e)
  }
}
```

### Call sites

```silt
fn load_config(path: String) -> Result(Config, AppError) {
  let raw = result.map_err(io.read_file(path), AppError.from)?
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

## Without `From`

Two other shapes do the same without a trait with a parameter:

### 1. Keep `result.map_err` with closures

```silt
fn load_config(path: String) -> Result(Config, AppError) {
  let raw = result.map_err(io.read_file(path)) { e -> IoProblem(e) }?
  let config = result.map_err(json.parse(raw, Config)) { e -> JsonProblem(e) }?
  Ok(config)
}
```

Three extra characters per line (`{e->`). Works. Matches Gleam.

### 2. One trait per target AppError type

```silt
trait IntoAppError {
  fn into_app_error(self) -> AppError
}

trait IntoAppError for io.IoError {
  fn into_app_error(self) -> AppError {
    IoProblem(self)
  }
}

trait IntoAppError for json.JsonError {
  fn into_app_error(self) -> AppError {
    JsonProblem(self)
  }
}

fn load_config(path: String) -> Result(Config, AppError) {
  let raw = result.map_err(io.read_file(path), IntoAppError.into_app_error)?
  let cfg = result.map_err(json.parse(raw, Config), IntoAppError.into_app_error)?
  Ok(cfg)
}
```

Each source type has one impl, selected by `Self`. Drawback: one
`trait IntoX` per target AppError type — it doesn't reuse.

## What the compiler does

1. **An impl per trait argument.** An impl is keyed by its trait, the
   head of its type and the heads of its trait arguments: the two
   impls above are two, each with its own methods and dictionary.
   Two impls whose trait arguments have the same heads are a
   duplicate; an impl whose trait argument is a type variable
   (`trait From(e) for AppError`) is for every argument, the type's
   only impl of the trait.

2. **Selection by the trait argument.** `AppError.from(e)` with
   `e: io.IoError` reaches `From(io.IoError) for AppError`: the use
   owes `AppError: From(io.IoError)`, and that impl is the evidence.

3. **`AppError.from` as a value.** Its trait argument is what the
   context asks: passed to `result.map_err(r, AppError.from)`, the
   function type `map_err` expects (`Fn(io.IoError) -> f`) decides it.
   `let f = AppError.from` with nothing deciding it is an error
   ("cannot decide which impl of 'From(_)' for 'AppError' this use of
   'from' means"); `let f: Fn(io.IoError) -> AppError = AppError.from`
   decides it.

## Rejected alternatives

| Approach | Why rejected |
|----------|--------------|
| `.into()` method with target-type inference | The magic this proposal explicitly rejects — and even with a visible name the mechanism is near-identical. |
| `?` auto-conversion (Rust-style) | Compounds the above with a second inference layer. Explicitly ruled out in principles. |
| Compiler-inserted coercion on assignment | Whole-program magic; refactors produce surprising conversions. |
| `result.wrap_err(r, Constructor)` as the only mechanism | Fine as a helper, but doesn't centralize conversions — same variant may be passed at 20 call sites. |
| Unified `SiltError` stdlib type | Loses the module-specific pattern-match granularity the typed errors just shipped. |
| Polymorphic variants (OCaml-style) | Requires a structural-typing extension to silt's nominal type system. Out of scope. |
| **One trait per source type** (`trait IntoAppError { fn into_app_error(self) -> AppError }`) | Works, but doesn't scale — one new trait per target AppError. Documented above. |
| **Inverted `trait IntoErr(target) for Source`** | Dispatches by Self, with the target decided by the context. Rejected because call site becomes `IntoErr.into_err(io_err)` with target inferred from context — i.e. Rust's `.into()` with a different name, the very thing the proposal exists to reject. |

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

### Why not have the compiler write the impl?

Two reasons:
1. Silt has no general `#[derive]` machinery for user-declared
   traits. An impl of `From` written by the compiler would be a bespoke compiler
   feature that we'd have to justify on its own.
2. Even with a general derive system, derived `From` impls
   hide the wrap variant from the impl site. If `AppError` has
   `IoProblem(IoError)`, a derived `trait From(IoError) for AppError`
   obviously maps to `IoProblem(e)` — but the user has to know
   "derived From picks the single-variant wrap" to verify the
   mapping is what they want. Explicit impls keep the mapping on
   the page.

If `#[derive]` lands and proves itself for less-ambiguous traits
(Display, Equal, etc.), a derived `From` becomes a natural
follow-up. Not part of v1.

## Related work

- [`docs/stdlib/errors.md`](../stdlib/errors.md) — the module-local
  typed-error system this proposal layers on top of.
- [`examples/cross_module_errors.silt`](../../examples/cross_module_errors.silt) —
  canonical example of the `result.map_err`-based idiom.
