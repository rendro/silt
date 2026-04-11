---
title: "result / option"
section: "Standard Library"
order: 7
---

# result

Functions for transforming and querying `Result(a, e)` values without pattern
matching.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `flat_map` | `(Result(a, e), (a) -> Result(b, e)) -> Result(b, e)` | Chain fallible operations |
| `flatten` | `(Result(Result(a, e), e)) -> Result(a, e)` | Remove one nesting level |
| `is_err` | `(Result(a, e)) -> Bool` | True if Err |
| `is_ok` | `(Result(a, e)) -> Bool` | True if Ok |
| `map_err` | `(Result(a, e), (e) -> f) -> Result(a, f)` | Transform the error |
| `map_ok` | `(Result(a, e), (a) -> b) -> Result(b, e)` | Transform the success value |
| `unwrap_or` | `(Result(a, e), a) -> a` | Extract value or use default |


## `result.flat_map`

```
result.flat_map(r: Result(a, e), f: (a) -> Result(b, e)) -> Result(b, e)
```

If `r` is `Ok(v)`, calls `f(v)` and returns its result. If `r` is `Err`,
returns the `Err` unchanged. Useful for chaining fallible operations.

```silt
import int

import result
fn main() {
    let r = Ok("42")
        |> result.flat_map { s -> int.parse(s) }
    println(r)  -- Ok(42)
}
```


## `result.flatten`

```
result.flatten(r: Result(Result(a, e), e)) -> Result(a, e)
```

Collapses a nested Result. `Ok(Ok(v))` becomes `Ok(v)`, `Ok(Err(e))` becomes
`Err(e)`, and `Err(e)` stays `Err(e)`.

```silt
import result
fn main() {
    println(result.flatten(Ok(Ok(42))))         -- Ok(42)
    println(result.flatten(Ok(Err("oops"))))    -- Err("oops")
}
```


## `result.is_err`

```
result.is_err(r: Result(a, e)) -> Bool
```

Returns `true` if the result is an `Err`.

```silt
import result
fn main() {
    println(result.is_err(Err("fail")))  -- true
    println(result.is_err(Ok(42)))       -- false
}
```


## `result.is_ok`

```
result.is_ok(r: Result(a, e)) -> Bool
```

Returns `true` if the result is an `Ok`.

```silt
import result
fn main() {
    println(result.is_ok(Ok(42)))       -- true
    println(result.is_ok(Err("fail")))  -- false
}
```


## `result.map_err`

```
result.map_err(r: Result(a, e), f: (e) -> f) -> Result(a, f)
```

If `r` is `Err(e)`, returns `Err(f(e))`. If `r` is `Ok`, returns it unchanged.

```silt
import result
fn main() {
    let r = Err("not found") |> result.map_err { e -> "Error: {e}" }
    println(r)  -- Err("Error: not found")
}
```


## `result.map_ok`

```
result.map_ok(r: Result(a, e), f: (a) -> b) -> Result(b, e)
```

If `r` is `Ok(v)`, returns `Ok(f(v))`. If `r` is `Err`, returns it unchanged.

```silt
import result
fn main() {
    let r = Ok(21) |> result.map_ok { n -> n * 2 }
    println(r)  -- Ok(42)
}
```


## `result.unwrap_or`

```
result.unwrap_or(r: Result(a, e), default: a) -> a
```

Returns the `Ok` value, or `default` if the result is `Err`.

```silt
import result
fn main() {
    println(result.unwrap_or(Ok(42), 0))        -- 42
    println(result.unwrap_or(Err("fail"), 0))    -- 0
}
```


---

# option

Functions for transforming and querying `Option(a)` values without pattern
matching.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `flat_map` | `(Option(a), (a) -> Option(b)) -> Option(b)` | Chain optional operations |
| `is_none` | `(Option(a)) -> Bool` | True if None |
| `is_some` | `(Option(a)) -> Bool` | True if Some |
| `map` | `(Option(a), (a) -> b) -> Option(b)` | Transform the inner value |
| `to_result` | `(Option(a), e) -> Result(a, e)` | Convert to Result with error value |
| `unwrap_or` | `(Option(a), a) -> a` | Extract value or use default |


## `option.flat_map`

```
option.flat_map(opt: Option(a), f: (a) -> Option(b)) -> Option(b)
```

If `opt` is `Some(v)`, calls `f(v)` and returns its result. If `opt` is `None`,
returns `None`.

```silt
import option
fn main() {
    let chained = Some(42) |> option.flat_map { n ->
        match {
            n > 0 -> Some(n * 2)
            _ -> None
        }
    }
    println(chained)  -- Some(84)
}
```


## `option.is_none`

```
option.is_none(opt: Option(a)) -> Bool
```

Returns `true` if the option is `None`.

```silt
import option
fn main() {
    println(option.is_none(None))      -- true
    println(option.is_none(Some(1)))   -- false
}
```


## `option.is_some`

```
option.is_some(opt: Option(a)) -> Bool
```

Returns `true` if the option is `Some`.

```silt
import option
fn main() {
    println(option.is_some(Some(1)))   -- true
    println(option.is_some(None))      -- false
}
```


## `option.map`

```
option.map(opt: Option(a), f: (a) -> b) -> Option(b)
```

If `opt` is `Some(v)`, returns `Some(f(v))`. If `opt` is `None`, returns `None`.

```silt
import option
fn main() {
    let doubled = Some(21) |> option.map { n -> n * 2 }
    println(doubled)  -- Some(42)
}
```


## `option.to_result`

```
option.to_result(opt: Option(a), error: e) -> Result(a, e)
```

Converts `Some(v)` to `Ok(v)` and `None` to `Err(error)`.

```silt
import option
fn main() {
    let r = option.to_result(Some(42), "missing")
    println(r)  -- Ok(42)

    let r2 = option.to_result(None, "missing")
    println(r2)  -- Err("missing")
}
```


## `option.unwrap_or`

```
option.unwrap_or(opt: Option(a), default: a) -> a
```

Returns the inner value if `Some`, otherwise returns `default`.

```silt
import option
fn main() {
    println(option.unwrap_or(Some(42), 0))  -- 42
    println(option.unwrap_or(None, 0))      -- 0
}
```
