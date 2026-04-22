---
title: "stdlib errors"
section: "Standard Library"
order: 20
---

# Stdlib typed errors

Every fallible stdlib module declares its own typed error enum. Silt's
`Result(T, ModuleError)` return shape lets user code pattern-match the
failure modes directly instead of substring-matching on a `String`
payload. Each enum implements the built-in `Error` trait, which
supertypes `Display` and provides a `message()` method so code that
just wants a rendered error can fall back to `"{e.message()}"`.

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

### `TcpError` (requires `import tcp`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `TcpConnect(String)` | message | tcp/dns connect failure |
| `TcpTls(String)` | message | TLS handshake failure |
| `TcpClosed` | — | connection closed (broken pipe, peer reset) |
| `TcpTimeout` | — | op exceeded its deadline |
| `TcpUnknown(String)` | message | unclassified socket failure |

### `PgError` (requires `import postgres`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `PgConnect(String)` | message | tcp / DNS / pool checkout failure |
| `PgTls(String)` | message | TLS setup / handshake |
| `PgAuthFailed(String)` | message | SQLSTATE class 28 |
| `PgQuery(String, String)` | message, SQLSTATE | server-reported error |
| `PgTypeMismatch(String, String, String)` | column, expected, actual | row decode |
| `PgNoSuchColumn(String)` | column | row.get on missing column |
| `PgClosed` | — | connection dropped mid-query |
| `PgTimeout` | — | statement / pool timeout |
| `PgTxnAborted` | — | SQLSTATE 25P02 — rollback required |
| `PgUnknown(String)` | message | unclassified pg error |

### `TimeError` (requires `import time`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `TimeParseFormat(String)` | message | pattern did not match input |
| `TimeOutOfRange(String)` | message | field out of valid range (e.g. month=13) |

### `BytesError` (requires `import bytes`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `BytesInvalidUtf8(Int)` | byte offset | decode failed at offset |
| `BytesInvalidHex(String)` | message | bad hex string |
| `BytesInvalidBase64(String)` | message | bad base64 string |
| `BytesByteOutOfRange(Int)` | value | list element outside 0..=255 |
| `BytesOutOfBounds(Int)` | index | slice or get index out of bounds |

### `ChannelError` (requires `import channel`)

Returned by `channel.recv_timeout`.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `ChannelTimeout` | — | timer elapsed before a value arrived |
| `ChannelClosed` | — | channel closed with no more values |

### `RegexError` (requires `import regex`)

Constructible by user code. Stdlib `regex.*` functions do not return
`Result(_, RegexError)` — their signatures return `Bool` / `Option` /
`List` / `String`, and an invalid pattern surfaces as a runtime error
at the call site rather than through `Err`.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `RegexInvalidPattern(String, Int)` | message, byte offset | pattern did not parse |
| `RegexTooBig` | — | compiled pattern exceeded size budget |

## Stdlib functions that return `Result(T, String)`

A handful of fallible stdlib functions surface their error as a plain
`String` rather than a typed enum — the failure modes are not diverse
enough to benefit from a richer taxonomy:

- `encoding.url_decode`, `encoding.form_decode`
- `crypto.random_bytes`
- `uuid.parse`

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

type Config { host: String, port: Int }

type AppError {
  IoProblem(IoError),
  JsonProblem(JsonError),
}

fn load_config(path: String) -> Result(Config, AppError) {
  let raw = io.read_file(path) |> result.map_err(IoProblem)?
  let cfg = json.parse(raw, Config) |> result.map_err(JsonProblem)?
  Ok(cfg)
}
```

`?` binds looser than `|>`, so the whole pipeline is a single expression
terminated by `?`. See [`examples/cross_module_errors.silt`](../../examples/cross_module_errors.silt)
for a longer walkthrough. A separate proposal
([`error-from-trait.md`](../proposals/error-from-trait.md)) tracks the
design for a `.into()`-based ergonomics layer over this pattern.
