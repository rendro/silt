---
title: "int / float"
section: "Standard Library"
order: 6
---

# int

Functions for parsing, converting, and comparing integers.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `abs` | `(Int) -> Int` | Absolute value |
| `clamp` | `(Int, Int, Int) -> Int` | Clamp value to `[lo, hi]` |
| `max` | `(Int, Int) -> Int` | Larger of two values |
| `min` | `(Int, Int) -> Int` | Smaller of two values |
| `parse` | `(String) -> Result(Int, ParseError)` | Parse string to integer |
| `to_float` | `(Int) -> Float` | Convert to float |
| `to_string` | `(Int) -> String` | Convert to string |


## `int.abs`

```
int.abs(n: Int) -> Int
```

Returns the absolute value. Runtime error if `n` is `Int` minimum
(`-9223372036854775808`) since the result cannot be represented.

```silt
import int
fn main() {
    println(int.abs(-42))  -- 42
    println(int.abs(7))    -- 7
}
```


## `int.clamp`

```
int.clamp(x: Int, lo: Int, hi: Int) -> Int
```

Returns `x` constrained to the inclusive range `[lo, hi]`: `lo` if
`x < lo`, `hi` if `x > hi`, otherwise `x`. Runtime error if `lo > hi`
(invalid bounds).

```silt
import int
fn main() {
    println(int.clamp(5, 0, 10))    -- 5
    println(int.clamp(-3, 0, 10))   -- 0
    println(int.clamp(42, 0, 10))   -- 10
}
```


## `int.max`

```
int.max(a: Int, b: Int) -> Int
```

Returns the larger of two integers.

```silt
import int
fn main() {
    println(int.max(3, 7))  -- 7
}
```


## `int.min`

```
int.min(a: Int, b: Int) -> Int
```

Returns the smaller of two integers.

```silt
import int
fn main() {
    println(int.min(3, 7))  -- 3
}
```


## `int.parse`

```
int.parse(s: String) -> Result(Int, ParseError)
```

Parses a string as an integer. Leading/trailing whitespace is trimmed. Returns
`Ok(n)` on success, `Err(ParseError)` on failure — a typed enum with variants
`ParseEmpty`, `ParseInvalidDigit(offset)`, `ParseOverflow`, and `ParseUnderflow`.
Match on the variant to handle specific cases, or fall back to `e.message()` for
a human-readable default.

```silt
import int
fn main() {
    match int.parse("42") {
        Ok(n) -> println(n)
        Err(e) -> println("parse error: {e.message()}")
    }

    -- Pattern-match on specific failure modes:
    match int.parse("") {
        Ok(_) -> ()
        Err(int.ParseEmpty) -> println("cannot parse empty input")
        Err(int.ParseInvalidDigit(i)) -> println("bad digit at byte {i}")
        Err(int.ParseOverflow) -> println("too large")
        Err(int.ParseUnderflow) -> println("too small")
    }
}
```


## `int.to_float`

```
int.to_float(n: Int) -> Float
```

Converts an integer to a float.

```silt
import int
fn main() {
    let f = int.to_float(42)
    println(f)  -- 42
}
```


## `int.to_string`

```
int.to_string(n: Int) -> String
```

Converts an integer to its string representation.

```silt
import int
fn main() {
    let s = int.to_string(42)
    println(s)  -- 42
}
```


---

# float

Functions for parsing, rounding, converting, and comparing floats.

> **Floats are always finite:** a `Float` is never NaN or infinite. An
> operation whose result would be NaN or infinite raises a runtime error
> instead, the way `Int` overflow does: `x / 0.0` raises "float division by
> zero", `+`, `-` and `*` raise "float overflow", and `math.sqrt` of a
> negative number, `math.log` of a number that is not positive, and
> `math.pow` overflow raise too. Guard the input when it can be out of
> range: `match b { 0.0 -> 0.0, _ -> a / b }`.

> **Note:** `round`, `ceil`, and `floor` return `Float`, not `Int`. Use
> `float.to_int` to convert the result to an integer.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `abs` | `(Float) -> Float` | Absolute value |
| `ceil` | `(Float) -> Float` | Round up to nearest integer (as Float) |
| `clamp` | `(Float, Float, Float) -> Float` | Clamp value to `[lo, hi]` |
| `floor` | `(Float) -> Float` | Round down to nearest integer (as Float) |
| `max` | `(Float, Float) -> Float` | Larger of two values |
| `min` | `(Float, Float) -> Float` | Smaller of two values |
| `parse` | `(String) -> Result(Float, ParseError)` | Parse string to float |
| `round` | `(Float) -> Float` | Round to nearest integer (as Float) |
| `to_int` | `(Float) -> Int` | Truncate to integer |
| `to_string` | `(Float) -> String` | Shortest round-trippable representation |
| `to_string` | `(Float, Int) -> String` | Format with fixed decimal places |
| **Constants** | | |
| `float.max_value` | `Float` | Maximum finite value (`1.7976931348623157e+308`) |
| `float.min_value` | `Float` | Minimum finite value (`-1.7976931348623157e+308`) |
| `float.epsilon` | `Float` | Machine epsilon (`2.220446049250313e-16`) |
| `float.min_positive` | `Float` | Smallest positive normal (`2.2250738585072014e-308`) |


## `float.abs`

```
float.abs(f: Float) -> Float
```

Returns the absolute value.

```silt
import float
fn main() {
    println(float.abs(-3.14))  -- 3.14
}
```


## `float.ceil`

```
float.ceil(f: Float) -> Float
```

Rounds up to the nearest integer, returned as a Float.

```silt
import float
fn main() {
    println(float.ceil(3.2))   -- 4
    println(float.ceil(-3.2))  -- -3
}
```


## `float.clamp`

```
float.clamp(x: Float, lo: Float, hi: Float) -> Float
```

Returns `x` constrained to the inclusive range `[lo, hi]`: `lo` if
`x < lo`, `hi` if `x > hi`, otherwise `x`. Runtime error if `lo > hi`
(invalid bounds).

```silt
import float
fn main() {
    println(float.clamp(0.5, 0.0, 1.0))   -- 0.5
    println(float.clamp(-0.2, 0.0, 1.0))  -- 0
    println(float.clamp(1.5, 0.0, 1.0))   -- 1
}
```


## `float.floor`

```
float.floor(f: Float) -> Float
```

Rounds down to the nearest integer, returned as a Float.

```silt
import float
fn main() {
    println(float.floor(3.9))   -- 3
    println(float.floor(-3.2))  -- -4
}
```


## `float.max`

```
float.max(a: Float, b: Float) -> Float
```

Returns the larger of two floats.

```silt
import float
fn main() {
    println(float.max(1.5, 2.5))  -- 2.5
}
```


## `float.min`

```
float.min(a: Float, b: Float) -> Float
```

Returns the smaller of two floats.

```silt
import float
fn main() {
    println(float.min(1.5, 2.5))  -- 1.5
}
```


## `float.parse`

```
float.parse(s: String) -> Result(Float, ParseError)
```

Parses a string as a float. Leading/trailing whitespace is trimmed. Returns
`Ok(f)` on success, `Err(ParseError)` on failure — the same typed enum
`int.parse` uses (`ParseEmpty`, `ParseInvalidDigit(offset)`, `ParseOverflow`,
`ParseUnderflow`). A `Float` is always finite, so `"NaN"` and `"inf"` are
rejected as `ParseInvalidDigit(0)`, and a literal outside the `Float` range
as `ParseOverflow` (`"1e400"`) or `ParseUnderflow` (`"-1e400"`).

```silt
import float
fn main() {
    match float.parse("3.14") {
        Ok(f) -> println(f)
        Err(e) -> println("error: {e.message()}")
    }
}
```


## `float.round`

```
float.round(f: Float) -> Float
```

Rounds to the nearest integer, returned as a Float. Ties round away from zero.

```silt
import float
fn main() {
    println(float.round(3.6))  -- 4
    println(float.round(3.4))  -- 3
}
```


## `float.to_int`

```
float.to_int(f: Float) -> Int
```

Truncates toward zero, converting to an integer. Raises a runtime error if
the value is outside the `Int` range.

```silt
import float
fn main() {
    println(float.to_int(3.9))   -- 3
    println(float.to_int(-3.9))  -- -3
}
```


## `float.to_string`

```
float.to_string(f: Float) -> String
float.to_string(f: Float, decimals: Int) -> String
```

Converts a float to its string representation.

- **One-argument form:** returns the shortest round-trippable
  representation. Whole-number floats always include a decimal point
  (`3.0` rather than `3`) so the result parses back as a float.
- **Two-argument form:** formats with exactly `decimals` decimal
  places. `decimals` must be a non-negative `Int`.

```silt
import float
fn main() {
    -- 1-arg form: shortest round-trippable
    println(float.to_string(3.14159))     -- 3.14159
    println(float.to_string(42.0))        -- 42.0

    -- 2-arg form: fixed decimal places
    println(float.to_string(3.14159, 2))  -- 3.14
    println(float.to_string(42.0, 0))     -- 42
}
```


## Float Constants

| Constant | Type | Value |
|----------|------|-------|
| `float.max_value` | `Float` | `1.7976931348623157e+308` |
| `float.min_value` | `Float` | `-1.7976931348623157e+308` |
| `float.epsilon` | `Float` | `2.220446049250313e-16` |
| `float.min_positive` | `Float` | `2.2250738585072014e-308` |
