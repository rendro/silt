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

## The `?` Operator

Propagates errors to the caller. Works on both `Result` and `Option`:

```silt
fn process(input) {
  let n = parse_int(input)?       -- returns Err early if parse fails
  let result = validate(n)?
  Ok(result * 2)
}
```

## `when`-`else` for Custom Errors

When you need custom error messages or destructuring beyond `?`:

```silt
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }
  let host = host_line |> string.replace("host=", "")

  when Some(port_line) = lines |> list.find { l -> string.contains(l, "port=") } else {
    return Err("missing port in config")
  }
  when Ok(port) = port_line |> string.replace("port=", "") |> int.parse() else {
    return Err("invalid port number")
  }

  Ok("connecting to {host}:{port}")
}
```

## Choosing Between `?` and `when`-`else`

Use `?` when you want to propagate the error unchanged. Use `when`-`else`
when you need to:

- Provide a custom error message
- Destructure something other than `Result` or `Option`
- Combine pattern and boolean guards in a flat sequence

```silt
-- Simple propagation: use ?
let value = parse(input)?

-- Custom error message: use when-else
when Ok(value) = parse(input) else {
  return Err("failed to parse input: expected integer")
}

-- Mixed pattern and boolean guards
fn process(input) {
  when Ok(value) = parse(input) else { return Err("parse failed") }
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
