---
title: "stdlib errors"
section: "Standard Library"
order: 20
---

# Stdlib typed errors

Phase 0 of the stdlib error redesign ships six typed error enums — one
per fallible stdlib module — that user code can construct and pattern
match today. See the design rationale in
[stdlib-errors proposal](../proposals/stdlib-errors.md).

Phase 1 of the redesign is migrating stdlib signatures from
`Result(T, String)` to `Result(T, ModuleError)` module-by-module.
Already landed: `io.*`, `fs.*`, `json.parse*`, `toml.*`, `int.parse`,
`float.parse`. The remaining modules (`http`, `regex`) still return
`Result(T, String)` today — users who want a typed handle wrap the
string errors in their own code until the relevant phase lands.

## Variant naming

Every variant is module-prefixed (`IoNotFound`, not `NotFound`) so
silt's one-variant-per-enum registration never collides. Each variant
is globally unique and may be constructed either bare or with its enum
as qualifier:

```silt
import io

let a = IoNotFound("config.toml")
let b = IoError.IoNotFound("config.toml")  -- same value
```

Construction is gated on the owning module being imported — bare
`IoNotFound(...)` without `import io` is a compile error. Pattern
matching is not gated: once you hold a value, you can destructure it
regardless of imports.

## Enums

### `IoError` (requires `import io`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `IoNotFound(String)` | path | file or directory missing |
| `IoPermissionDenied(String)` | path | permissions check failed |
| `IoAlreadyExists(String)` | path | target already exists |
| `IoInvalidInput(String)` | description | malformed argument |
| `IoInterrupted` | — | syscall interrupted |
| `IoUnexpectedEof` | — | reader hit EOF mid-record |
| `IoWriteZero` | — | writer returned zero bytes |
| `IoUnknown(String)` | platform message | unclassified platform error |

### `JsonError` (requires `import json`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `JsonSyntax(String, Int)` | message, byte offset | syntactically invalid JSON |
| `JsonTypeMismatch(String, String)` | expected, actual | wrong JSON type for target |
| `JsonMissingField(String)` | field name | required field absent |
| `JsonUnknown(String)` | message | unclassified parse failure |

### `TomlError` (requires `import toml`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `TomlSyntax(String, Int)` | message, byte offset | syntactically invalid TOML |
| `TomlTypeMismatch(String, String)` | expected, actual | wrong TOML type for target |
| `TomlMissingField(String)` | field name | required field absent |
| `TomlUnknown(String)` | message | unclassified parse failure |

### `ParseError` (requires `import int` or `import float`)

Shared by `int.parse` and `float.parse`. Either import unlocks the
variants; users who import only one can still match on values they
receive from the other.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `ParseEmpty` | — | input was empty |
| `ParseInvalidDigit(Int)` | byte offset | non-digit character at offset |
| `ParseOverflow` | — | value exceeds type max |
| `ParseUnderflow` | — | value below type min |

### `HttpError` (requires `import http`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `HttpConnect(String)` | message | TCP / DNS connect failure |
| `HttpTls(String)` | message | TLS handshake / cert failure |
| `HttpTimeout` | — | request exceeded its deadline |
| `HttpInvalidUrl(String)` | url | URL did not parse |
| `HttpInvalidResponse(String)` | message | response violated protocol |
| `HttpClosedEarly` | — | peer closed before response completed |
| `HttpStatusCode(Int, String)` | status, body preview | non-success status |
| `HttpUnknown(String)` | message | unclassified transport error |

### `RegexError` (requires `import regex`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `RegexInvalidPattern(String, Int)` | message, byte offset | pattern did not parse |
| `RegexTooBig` | — | compiled pattern exceeded size budget |

## Example: user-side pattern matching

```silt
import io

fn handle(e: IoError) -> String {
  match e {
    IoNotFound(path) -> "missing: {path}"
    IoPermissionDenied(path) -> "denied: {path}"
    IoAlreadyExists(_) | IoInvalidInput(_) -> "recoverable"
    IoInterrupted | IoUnexpectedEof | IoWriteZero -> "transient"
    IoUnknown(msg) -> "unknown: {msg}"
  }
}

fn main() {
  println(handle(IoNotFound("config.toml")))
}
```

Cross-module composition follows the pattern from the proposal: wrap
each module's error in an `AppError` enum and use `result.map_err`.
The stdlib does not auto-convert between error types.
