---
title: "stdlib errors"
section: "Standard Library"
order: 24
---

# Stdlib typed errors

Every fallible stdlib module declares its own typed error enum. Silt's
`Result(T, ModuleError)` return shape lets user code pattern-match the
failure modes directly instead of substring-matching on a `String`
payload. Each enum implements the built-in `Error` trait, which
supertypes `Display` and provides a `message()` method so code that
just wants a rendered error can fall back to `"{e.message()}"`.

## Variant naming

Every error enum is named after its module (`io.IoError`,
`tcp.TcpError`), its variants carry no prefix (`io.NotFound`,
`tcp.Timeout`), and its last variant is `Other(String)` for a failure
that fits no other. An error enum and its variants are members of their
module, reached through it, with or without the enum as a second
qualifier:

```silt
import io

let a = io.NotFound("config.toml")

let b = io.IoError.NotFound("config.toml") -- same value
```

A selective import binds a variant by its bare name:
`import io.{ NotFound }`. Two modules may have a variant of one name
(`tcp.Timeout` and `http.Timeout`, `Other` in every module): a file
imports at most one of them bare, and writes the others through their
modules.

## Enums

### `IoError` (requires `import io`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `NotFound(String)` | path | file or directory missing |
| `PermissionDenied(String)` | path | permissions check failed |
| `AlreadyExists(String)` | path | target already exists |
| `InvalidInput(String)` | description | malformed argument |
| `Interrupted` | — | syscall interrupted |
| `UnexpectedEof` | — | reader hit EOF mid-record |
| `WriteZero` | — | writer returned zero bytes |
| `Other(String)` | platform message | unclassified platform error |

### `JsonError` (requires `import json`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Syntax(String, Int)` | message, byte offset | syntactically invalid JSON |
| `TypeMismatch(String, String)` | expected, actual (ending with the value's path) | wrong JSON type for target |
| `MissingField(String)` | the field's path | required field absent |
| `Other(String)` | message | unclassified parse failure |

### `TomlError` (requires `import toml`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Syntax(String, Int)` | message, byte offset | syntactically invalid TOML |
| `TypeMismatch(String, String)` | expected, actual (ending with the value's path) | wrong TOML type for target |
| `MissingField(String)` | the field's path | required field absent |
| `Other(String)` | message | unclassified parse failure |

### `IntError` (requires `import int`)

Returned by `int.parse`.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Empty` | — | input was empty |
| `InvalidDigit(Int)` | byte offset | non-digit character at offset |
| `Overflow` | — | value exceeds type max |
| `Underflow` | — | value below type min |
| `Other(String)` | message | unclassified parse failure |

### `FloatError` (requires `import float`)

Returned by `float.parse`: the variants of `IntError` and `NotFinite`,
in a type of its own (`float.Empty` is not `int.Empty`).

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Empty` | — | input was empty |
| `InvalidDigit(Int)` | byte offset | character that is no part of a number at offset |
| `Overflow` | — | literal above the `Float` range |
| `Underflow` | — | literal below the `Float` range |
| `NotFinite` | — | a spelled-out infinity or NaN (`"inf"`, `"NaN"`): a `Float` is always finite |
| `Other(String)` | message | unclassified parse failure |

### `HttpError` (requires `import http`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Connect(String)` | message | no connection was made: refused, no route, no such host |
| `Tls(String)` | message | TLS handshake / cert failure |
| `Timeout` | — | one of the request's times passed |
| `InvalidUrl(String)` | url | the URL cannot be read as one, has no host, has a scheme other than `http` and `https`, or names a port that is none |
| `InvalidResponse(String)` | message | what the server sent is no HTTP, or its body is larger than 10,000,000 bytes |
| `ClosedEarly` | — | the server went away before the response was complete |
| `Other(String)` | message | unclassified transport error |

### `TcpError` (requires `import tcp`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Connect(String)` | message | `tcp.connect` made no connection (refused, unreachable, no such host) |
| `Bind(String)` | message | `tcp.listen` bound no listener (address in use, not this machine's, no address) |
| `Tls(String)` | message | TLS handshake failure |
| `Closed` | — | connection closed, by this end or the other (also a reset, an abort, a broken pipe) |
| `Timeout` | — | op exceeded its deadline |
| `Other(String)` | message | unclassified socket failure |

### `PostgresError` (requires `import postgres`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Connect(String)` | message | a connect that failed (refused port, unknown or unreachable host, missing database), URL parse, connect options, SQLSTATE class 08 |
| `Tls(String)` | message | TLS handshake / a server without TLS when `sslmode` requires TLS / cert read / connector build |
| `AuthFailed(String)` | message | SQLSTATE class 28: a role or password the server refuses |
| `Query(String, String)` | message, SQLSTATE | server-reported error |
| `TypeMismatch(String, String, String)` | column, expected, actual | a column whose type, or a cell whose value, does not fit the field it is decoded into |
| `NoSuchColumn(String)` | column | a field with no column in the result; SQLSTATE 42703 |
| `Closed` | — | connection dropped, or a transaction used after it ended |
| `Timeout` | — | SQLSTATE 57014, a server that does not answer a connect, no free connection in the pool's 30 s |
| `TxnAborted` | — | SQLSTATE 25P02 — rollback required |
| `Other(String)` | message | unclassified pg error |

### `TimeError` (requires `import time`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `ParseFormat(String)` | message | the text did not match a pattern that could have given the value |
| `OutOfRange(String)` | message | field out of valid range (e.g. month=13) |
| `InvalidFormat(String)` | what is wrong | the pattern of `time.format`, `time.format_date`, `time.parse` or `time.parse_date` is no format, or one from which no text could give the value (no time of day for a `DateTime`, no date, empty) |
| `Other(String)` | message | unclassified failure |

### `BytesError` (requires `import bytes`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `InvalidUtf8(Int)` | byte offset | decode failed at offset |
| `InvalidHex(String)` | message | bad hex string |
| `InvalidBase64(String)` | message | bad base64 string |
| `ByteOutOfRange(Int)` | value | list element outside 0..=255 |
| `Other(String)` | message | unclassified failure |

### `TaskError` (requires `import task`)

Returned by `task.deadline`.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Timeout` | — | the function was ended because its time was over |
| `Other(String)` | message | unclassified failure |

### `EncodingError` (requires `import encoding`)

Returned by `encoding.url_decode` and `encoding.form_decode`.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `InvalidPercent(String)` | what and where | a `%` that two hex digits do not follow |
| `InvalidUtf8(String)` | where | decoded bytes are not valid UTF-8 |
| `Other(String)` | message | unclassified failure |

### `CryptoError` (requires `import crypto`)

Returned by `crypto.random_bytes`.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `InvalidLength(Int)` | the length asked for | negative, or above the 1 MiB cap |
| `Other(String)` | message | the OS CSPRNG failed |

### `UuidError` (requires `import uuid`)

Returned by `uuid.parse`.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Invalid(String)` | why | not a syntactically valid UUID |
| `Other(String)` | message | unclassified failure |

### `RegexError` (requires `import regex`)

Returned by every `regex.*` function for a pattern that does not compile.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `InvalidPattern(String, Int)` | what is wrong, and the character of the pattern where: counted from 0, as `string.slice` counts | pattern did not parse |
| `TooBig` | — | compiled pattern exceeded size budget |
| `Other(String)` | message | unclassified failure |

## Example: user-side pattern matching

```silt
import io

fn handle(e: io.IoError) -> String {
  match e {
    io.NotFound(path) -> "missing: {path}"
    io.PermissionDenied(path) -> "denied: {path}"
    io.AlreadyExists(_) | io.InvalidInput(_) -> "recoverable"
    io.Interrupted | io.UnexpectedEof | io.WriteZero -> "transient"
    io.Other(msg) -> "unknown: {msg}"
  }
}

fn main() {
  println(handle(io.NotFound("config.toml")))
}
```

## Cross-module composition

Silt does not auto-convert between error types. A function that spans
several stdlib modules wraps each module's error in a local `AppError`
enum and lifts each call's `Err` into it with `result.map_err`. Variant
constructors are first-class `Fn(e) -> f` values, so the second argument
is just the constructor name — no closure wrapper needed:

```silt
import io
import json
import result

type Config {
  host: String,
  port: Int,
}

type AppError {
  IoProblem(io.IoError),
  JsonProblem(json.JsonError),
}

fn load_config(path: String) -> Result(Config, AppError) {
  let raw = io.read_file(path) |> result.map_err(IoProblem)?
  let cfg = json.parse(raw, Config) |> result.map_err(JsonProblem)?
  Ok(cfg)
}
```

A `?` that ends a pipeline applies to the whole pipeline, so the pipeline
is a single expression terminated by `?`. See [`examples/cross_module_errors.silt`](../../examples/cross_module_errors.silt)
for a longer walkthrough. A separate proposal
([`error-from-trait.md`](../proposals/error-from-trait.md)) tracks the
design for a `.into()`-based ergonomics layer over this pattern.
