---
title: "Proposal: Typed stdlib errors"
section: "Proposals"
status: draft
---

# Typed stdlib errors

**Status:** proposal, not yet implemented.
**Scope:** every stdlib function that currently returns
`Result(T, String)`.

## Problem

Today every stdlib function that can fail uses `String` as the error
type. This was an expedient v0 choice. It has real costs:

```silt
match io.read_file(path) {
  Ok(contents) -> use(contents)
  Err(msg) -> {
    -- We want to distinguish "file not found" from "permission denied"
    -- but our only handle is the English message string:
    match {
      string.contains(msg, "not found") -> create_default()
      string.contains(msg, "permission") -> log_warning(msg)
      _ -> panic(msg)
    }
  }
}
```

Users resort to substring matching on diagnostic text. Message wording
is not part of the API contract, so changing a message breaks callers
silently. The community has called this the single biggest stdlib wart
(confirmed by the Phase 1/2 audit).

## Goals

1. **Pattern-matchable errors.** Error handling uses silt's native
   control flow (`match` on enum variants), not string inspection.
2. **Zero surprise from message wording.** Changing a `Display`
   formatting string must never break a program.
3. **Composable.** User code that calls several stdlib modules should
   be able to handle their errors together.
4. **Stays silty.** No exceptions, no trait objects, no new keywords.
   Only existing enums, `Result`, `match`, and auto-derived `Display`.
5. **Migration survivable.** The existing stdlib is large; the
   proposal must admit a phased rollout without breaking every
   existing silt program on day one.

## Non-goals

- Stack traces. Silt's errors are values, not exceptions — backtraces
  are a separate topic (likely a VM feature, not a stdlib type).
- A universal error trait. Silt has no trait objects; dynamic dispatch
  on errors would drag in a whole new subsystem.
- Error chains / `source()` (Rust's `Error::source`). Nice-to-have,
  not essential for v1.

## Options considered

### Option A — keep `String`

Status quo. Rejected: this proposal exists because users hit this daily.

### Option B — one unified `StdlibError` enum

```silt
type StdlibError {
  Io(IoKind, String)
  Json(JsonKind, String)
  Network(NetKind, String)
  ...
}
```

**Pros:** every stdlib function returns the same error type; composes
trivially; one `match` arm per module.

**Cons:** monolithic — every new stdlib error tag touches the central
enum. User-defined errors still need their own enum. Doesn't scale
past v1.

### Option C — per-module enums (recommended)

Each stdlib module defines its own error enum. The variants are
concrete failure modes.

```silt
-- in io
type IoError {
  NotFound(String)          -- path
  PermissionDenied(String)
  AlreadyExists(String)
  InvalidInput(String)
  Interrupted
  UnexpectedEof
  WriteZero
  Unknown(String)           -- fallback, carries platform message
}

-- in json
type JsonError {
  SyntaxError(String, Int)  -- message, byte offset
  UnexpectedType(String)    -- "expected Int, got String"
  MissingField(String)      -- field name
  Custom(String)
}
```

**Pros:** modular, extensible without touching other modules, matches
how Rust / Go structure their error types. Each module owns its
taxonomy.

**Cons:** composition across modules requires a user-defined wrapper
enum. Migration is per-module.

### Option D — structured record

```silt
type Error {
  code: String         -- "io.not_found"
  message: String
  context: Map(String, String)
}
```

**Pros:** one type, composable.

**Cons:** `code` is stringly-typed — we've just pushed the substring
problem down one level. Half-measure.

### Option E — trait-based

Rejected: silt has no trait objects or existentials. Adding them for
errors alone is disproportionate.

## Recommendation

**Option C — per-module enums.**

It matches silt's existing values-as-errors philosophy, uses only
machinery silt already has (enums, match, auto-derived `Display`), and
scales cleanly as the stdlib grows.

For cross-module composition, users wrap:

```silt
type AppError {
  IoProblem(IoError)
  JsonProblem(JsonError)
  Custom(String)
}

fn load_config(path: String) -> Result(Config, AppError) {
  let raw = io.read_file(path) |> result.map_err(IoProblem)?
  let config = json.parse(raw, Config) |> result.map_err(JsonProblem)?
  Ok(config)
}
```

The boilerplate is real but visible and typed. It's the same pattern
Rust's `thiserror` encourages and that silt's existing enum + `?`
already support without new language features.

## Taxonomy sketch

These are starting points, not final lists. Each module's enum grows
as we encounter real failure modes.

### `io`, `fs`
```silt
type IoError {
  NotFound(String)
  PermissionDenied(String)
  AlreadyExists(String)
  InvalidInput(String)
  NotADirectory(String)
  IsADirectory(String)
  Interrupted
  UnexpectedEof
  WriteZero
  Other(String)
}
```

### `json`, `toml`
```silt
type JsonError {
  Syntax(String, Int)        -- message, byte offset
  UnexpectedType(String, String)  -- expected, actual
  MissingField(String)       -- field name
  Custom(String)
}
```

### `http`
```silt
type HttpError {
  Connect(String)            -- host resolution / TCP connect failure
  Tls(String)
  Timeout
  InvalidUrl(String)
  InvalidResponse(String)
  ClosedEarly
  StatusCode(Int, String)    -- status, body preview
  Unknown(String)
}
```

### `int`, `float` — parse errors
```silt
type ParseError {
  Empty
  InvalidDigit(Int)          -- byte offset
  Overflow
  Underflow
}
```

### `regex`
```silt
type RegexError {
  InvalidPattern(String, Int) -- message, position
  TooBig
}
```

### Modules without failure modes

Many stdlib modules already return `Option` or infallible types
(`list`, `map`, `set`, most of `string`, `math`). They don't need an
error enum — the typed-Result conversion doesn't apply.

## Migration plan

Phased; the design explicitly supports partial rollout. Each phase
ships independently and runs green on its own:

1. **Phase 0 — add enums.** Declare every error enum in the stdlib
   without changing any function signatures. Each error type is
   usable by user code before any migration happens.

2. **Phase 1 — migrate high-use modules.** `io`, `fs`, `json`, `toml`,
   `int`, `float`. Every function changes from
   `Result(T, String)` to `Result(T, ModuleError)`. Every existing
   silt example and test updates accordingly.

3. **Phase 2 — migrate network + data modules.** `http`, `tcp`,
   `regex`, `encoding`, `crypto`, `uuid`.

4. **Phase 3 — migrate less-common modules.** `postgres`, `env`,
   `time`, whatever's left.

5. **Phase 4 — audit examples, docs, and user-visible error messages.**
   Ensure `Display` renders the new enums cleanly.

Each phase is a committed breaking change. The commit message MUST
spell out the migration recipe for any user code in the wild:
> "Before: `match e { Err(s) -> ... }`. After:
> `match e { Err(IoError.NotFound(p)) -> ..., Err(e) -> ... }`."

## Auto-derived `Display`

Silt already auto-derives `Display` for every user-defined type. The
new error enums inherit that automatically, so existing
`println("{err}")` sites keep working — the output shape just becomes
`IoError.NotFound("config.toml")` instead of a bespoke message. For
nicer output, each module can define an explicit
`trait Display for IoError` impl that formats like the current
messages. Decide per-module.

## Decisions

- **`Unknown(String)` fallback** (renamed from `Other`): kept on every
  module enum for now. New platform errors appear faster than we can
  enumerate them; `Unknown` is a pressure-release valve that lets the
  stdlib surface messages it can't classify without forcing a library
  version bump every time. Once each module's API stabilizes and the
  catalog of real-world failure modes is well-known, individual
  `Unknown(_)` payloads can be promoted to named variants; `Unknown`
  itself stays as a catch-all.

- **No auto-wrap on `?`.** Explicit-over-implicit. `?` only propagates
  when the callee's error type already matches the caller's return
  type. For cross-module composition, the user writes
  `result.map_err(Wrap)?` — visible and typed.

- **Explore a `From`-like trait for error conversion.** Tracked as a
  separate language-level design question (not part of this proposal).
  Goal would be an explicit `.into()` / `convert` call at the point
  of conversion, reducing `map_err(Wrap)` boilerplate without the
  implicit-propagation footgun `?` would introduce. Any such trait
  should be an opt-in user-declared conversion, not a compiler-inserted
  coercion.

- **Backtrace support: out of scope.** Separate VM-level feature;
  unrelated to the error-type design. May appear as a future debugger
  capability but not as part of `Result(T, E)`.

- **`panic` unchanged.** `panic` is fatal — no typed error. The typed
  error design applies only to recoverable failures surfaced through
  `Result`. Panics continue to abort with their string message.

## Cost estimate

- Phase 0 (enum declarations): 2-3 hours
- Phase 1 (migrate high-use modules): 6-8 hours
- Phase 2 (migrate network/data): 4-6 hours
- Phase 3 (migrate remainder): 2-3 hours
- Phase 4 (docs + examples polish): 2 hours

Total: approximately two focused days. Not shippable inside a single
session; each phase is a separate commit.

## When to implement

Not blocking any current user work. Suggest scheduling after:
- Release engineering is in shape (so migration pain affects fewer
  users)
- The generics work stabilizes (it shouldn't interact, but one fewer
  moving piece at a time)

Track via a single follow-up task. Start with Phase 0 (cheapest,
unblocks experimentation) whenever slack appears.
