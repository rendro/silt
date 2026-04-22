---
title: "Error Handling"
section: "Language"
order: 4
---

# Error Handling

## No Exceptions

Functions that can fail return `Result(value, error)`. Values that might be
absent are `Option(value)`. Errors are values -- visible in types, mandatory
to handle:

```silt
match parse_int(input) {
  Ok(n) -> use(n)
  Err(e) -> handle(e)
}
```

## Typed Stdlib Errors

Every fallible stdlib module except `crypto`, `encoding`, `uuid`, and `regex`
returns a typed error enum — `IoError`, `JsonError`, `TomlError`,
`ParseError`, `HttpError`, `TcpError`, `PgError`, `TimeError`, `BytesError`,
and `ChannelError`. Each variant is module-prefixed so pattern names never
collide: `IoNotFound`, `JsonSyntax`, `HttpTimeout`, and so on. The full
catalog lives in [`stdlib/errors.md`](../stdlib/errors.md).

You can either pattern-match on a specific variant, or fall back to
`.message()` for a rendered string:

```silt
import io
import string

fn main() {
  match io.read_file("app.json") {
    Ok(content) -> println("loaded {string.length(content)} bytes")
    Err(IoNotFound(path)) -> println("file does not exist: {path}")
    Err(IoPermissionDenied(path)) -> println("denied: {path}")
    Err(e) -> println("error: {e.message()}")
  }
}
```

Every stdlib error enum implements the built-in `Error` trait, which has
`Display` as a supertrait and provides `message(self) -> String`. That means
`"{e}"` string interpolation works too — `Display` gives you the same
rendered message.

## The `?` Operator

`?` propagates errors to the caller unchanged. It works on both `Result` and
`Option`, and it does **not** convert between error types — the inner `Err`
type of the `?`-ed expression must match the enclosing function's `Err`
type:

```silt
import io

fn read_head(path: String) -> Result(String, IoError) {
  let content = io.read_file(path)?     -- Err(IoError) propagates
  Ok(content)
}
```

### `?` and the pipe operator

`?` binds one step looser than `|>`, so a trailing `?` applies to the whole
pipeline — no parentheses needed:

```silt
import io
import result

type Wrap { Wrap(IoError) }

fn main() -> Result(String, Wrap) {
  let raw = io.read_file("config.toml") |> result.map_err({ e -> Wrap(e) })?
  Ok(raw)
}
```

`x |> f |> g?` parses as `(x |> f |> g)?`. On the other side, `produce()? |> double`
still works too — the `?` on the left is already attached by the time the
pipe sees the value.

Arithmetic operators (`+`, `-`, `*`, `/`, `%`), `..` (range), and `as` bind
tighter than `?`, so `x + y?` parses as `(x + y)?`. Comparison (`==`, `!=`,
`<`, `>`, `<=`, `>=`), boolean (`&&`, `||`), and `else` bind looser, so
`a == b?` is still `a == (b?)`.

## Cross-Module Error Composition

`?` does not convert between typed errors, so a function that calls several
stdlib modules — each returning its own error enum — needs a local wrapper.
The canonical pattern: declare an `AppError` enum, lift each call via
`result.map_err`, and let `?` propagate uniformly:

```silt
import io
import json
import http
import result

type Config { api_url: String, api_key: String }

type AppError {
  ConfigRead(IoError),
  ConfigParse(JsonError),
  ApiCall(HttpError),
}

fn load_and_fetch(path: String) -> Result(String, AppError) {
  let contents = result.map_err(io.read_file(path), { e -> ConfigRead(e) })?
  let config = result.map_err(json.parse(contents, Config), { e -> ConfigParse(e) })?
  let resp = result.map_err(http.get(config.api_url), { e -> ApiCall(e) })?
  Ok(resp.body)
}
```

Variant constructors like `Wrap` or `ConfigRead` are first-class `Fn` values,
so `result.map_err(r, ConfigRead)` also works with no closure wrapping.

See [`examples/cross_module_errors.silt`](../../examples/cross_module_errors.silt)
for the full worked example.

## `when let`-`else` for Custom Errors

When you need a custom error message or destructuring that goes beyond `?`:

```silt
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when let Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }
  let host = host_line |> string.replace("host=", "")

  when let Some(port_line) = lines |> list.find { l -> string.contains(l, "port=") } else {
    return Err("missing port in config")
  }
  when let Ok(port) = port_line |> string.replace("port=", "") |> int.parse() else {
    return Err("invalid port number")
  }

  Ok("connecting to {host}:{port}")
}
```

## Choosing Between `?` and `when let`-`else`

Use `?` when you want to propagate the error unchanged. Use `when let`-`else`
when you need to:

- Provide a custom error message
- Destructure something other than `Result` or `Option`
- Combine pattern and boolean guards in a flat sequence

```silt
-- Simple propagation: use ?
let value = parse(input)?

-- Custom error message: use when let-else
when let Ok(value) = parse(input) else {
  return Err("failed to parse input: expected integer")
}

-- Mixed pattern and boolean guards
fn process(input) {
  when let Ok(value) = parse(input) else { return Err("parse failed") }
  when value > 0 else { return Err("must be positive") }
  Ok(value * 2)
}
```

## Never Type

`return` and `panic()` produce the `Never` type, which unifies with any
type. This lets them appear in any expression position:

```silt
fn get_or_die(opt) {
  match opt {
    Some(v) -> v
    None -> panic("expected a value")   -- Never unifies with v's type
  }
}
```

## Result and Option Utilities

```silt
result.map_ok(Ok(1), fn(x) { x + 1 })        -- Ok(2)
result.flat_map(Ok(1), fn(x) { Ok(x + 1) })   -- Ok(2)
result.unwrap_or(Err("x"), 0)                  -- 0

option.map(Some(1), fn(x) { x + 1 })          -- Some(2)
option.flat_map(Some(1), fn(x) { Some(x + 1) })  -- Some(2)
option.unwrap_or(None, 0)                      -- 0
```

`result.flat_map` is symmetric with `option.flat_map` -- both take a value
and a function that returns a wrapped result, and flatten the nesting.
