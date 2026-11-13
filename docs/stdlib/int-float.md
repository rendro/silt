---
title: "int / float"
section: "Standard Library"
order: 6
---

# int

Functions for parsing, converting, and comparing integers, for powers, for
the bits of an integer, and the limits of `Int`.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `abs` | `(Int) -> Int` | Absolute value |
| `bit_and` | `(Int, Int) -> Int` | The bits set in both |
| `bit_or` | `(Int, Int) -> Int` | The bits set in either |
| `bit_xor` | `(Int, Int) -> Int` | The bits set in one but not both |
| `clamp` | `(Int, Int, Int) -> Int` | Clamp value to `[lo, hi]` |
| `max` | `(Int, Int) -> Int` | Larger of two values |
| `min` | `(Int, Int) -> Int` | Smaller of two values |
| `parse` | `(String) -> Result(Int, IntError)` | Parse string to integer |
| `parse_radix` | `(String, Int) -> Result(Int, IntError)` | Parse string to integer in a radix from 2 to 36 |
| `pow` | `(Int, Int) -> Int` | Power; overflow is a runtime error |
| `shift_left` | `(Int, Int) -> Int` | Bits moved up by count; bits past the top are gone |
| `shift_right` | `(Int, Int) -> Int` | Bits moved down by count, the sign kept |
| `to_radix` | `(Int, Int) -> String` | Text of the integer in a radix from 2 to 36 |
| **Constants** | | |
| `int.max_value` | `Int` | Largest Int (`9223372036854775807`) |
| `int.min_value` | `Int` | Smallest Int (`-9223372036854775808`) |


## `int.abs`

```
int.abs(n: Int) -> Int
```

Returns the absolute value. Runtime error if `n` is `Int` minimum
(`-9223372036854775808`) since the result cannot be represented.

```silt
import int

fn main() {
  println(int.abs(-42)) -- 42
  println(int.abs(7)) -- 7
}
```


## `int.bit_and`, `int.bit_or`, `int.bit_xor`

```
int.bit_and(a: Int, b: Int) -> Int
int.bit_or(a: Int, b: Int) -> Int
int.bit_xor(a: Int, b: Int) -> Int
```

The bits of the two numbers combined one by one, over all 64 bits of an
`Int` (a negative number has its high bits set, as in two's complement):
`bit_and` keeps a bit set in both, `bit_or` one set in either, `bit_xor` one
set in exactly one of them. `bit_xor(n, -1)` flips every bit. Use them for
masks, flags and checksums over the bytes of a `Bytes`.

```silt
import int

fn main() {
  println(int.bit_and(12, 10)) -- 8
  println(int.bit_or(12, 10)) -- 14
  println(int.bit_xor(12, 10)) -- 6
  println(int.bit_and(-1, 255)) -- 255
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
  println(int.clamp(5, 0, 10)) -- 5
  println(int.clamp(-3, 0, 10)) -- 0
  println(int.clamp(42, 0, 10)) -- 10
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
  println(int.max(3, 7)) -- 7
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
  println(int.min(3, 7)) -- 3
}
```


## `int.parse`

```
int.parse(s: String) -> Result(Int, IntError)
```

Parses a string as an integer. Leading/trailing whitespace is trimmed. Returns
`Ok(n)` on success, `Err(IntError)` on failure — a typed enum with variants
`Empty`, `InvalidDigit(offset)`, `Overflow` and `Underflow`.
Match on the variant to handle specific cases, or show it with `"{e}"` for
a human-readable default.

`offset` is the byte of `s` as written, the whitespace in front counted,
that cannot continue the number (`"1_000"` is `InvalidDigit(1)`,
`"  12x"` is `InvalidDigit(4)`), or the end of the number where it stops
short: a lone sign (`"-"` is `InvalidDigit(1)`). A text of nothing but
whitespace is `Empty`. `float.parse` counts the same way.

```silt
import int

fn main() {
  match int.parse("42") {
    Ok(n) -> println(n)
    Err(e) -> println("parse error: {e}")
  }

  -- Pattern-match on specific failure modes:
  match int.parse("") {
    Ok(_) -> ()
    Err(int.Empty) -> println("cannot parse empty input")
    Err(int.InvalidDigit(i)) -> println("bad digit at byte {i}")
    Err(int.Overflow) -> println("too large")
    Err(int.Underflow) -> println("too small")
  }
}
```


## `int.parse_radix`

```
int.parse_radix(s: String, radix: Int) -> Result(Int, IntError)
```

Parses a string as an integer written in `radix`, from 2 to 36: the digits
`0`-`9` and then the letters `a`-`z`, of either case. As for `int.parse`, the
whitespace around the number is trimmed, a `+` or `-` may come first, and a
failure is an `Err(IntError)` with the same variants and the same offsets.
There is no prefix: `"0xff"` is no number in radix 16. A `radix` outside 2 to
36 is a runtime error.

```silt
import int

fn main() {
  println(int.parse_radix("ff", 16)) -- Ok(255)
  println(int.parse_radix("-101", 2)) -- Ok(-5)
  println(int.parse_radix("12g", 16)) -- Err(invalid digit at byte 2)
}
```


## `int.pow`

```
int.pow(base: Int, exponent: Int) -> Int
```

Returns `base` to the power `exponent`, exactly. A result outside the `Int`
range is a runtime error, the overflow that `*` raises, and so is a negative
`exponent`, which has no `Int` result. `int.pow(0, 0)` is `1`.

```silt
import int

fn main() {
  println(int.pow(2, 10)) -- 1024
  println(int.pow(-3, 3)) -- -27
  println(int.pow(2, 62)) -- 4611686018427387904
}
```


## `int.shift_left`

```
int.shift_left(n: Int, count: Int) -> Int
```

Moves the 64 bits of `n` up by `count` places: 0s come in at the bottom and
the bits moved past the top are gone, with no error, since this is an
operation on bits and not arithmetic (use `int.pow` or `*` for a number that
must not overflow). So `int.shift_left(1, 63)` is `int.min_value`. A `count`
of 64 or more gives `0`, and a negative `count` counts as 0.

```silt
import int

fn main() {
  println(int.shift_left(1, 10)) -- 1024
  println(int.shift_left(255, 8)) -- 65280
  println(int.shift_left(1, 64)) -- 0
}
```


## `int.shift_right`

```
int.shift_right(n: Int, count: Int) -> Int
```

Moves the 64 bits of `n` down by `count` places, keeping the sign: the result
is `n` divided by 2^`count`, rounded down, so `int.shift_right(-7, 1)` is
`-4`. A `count` of 64 or more gives `0` for a number of 0 or more and `-1`
for a negative one, and a negative `count` counts as 0.

```silt
import int

fn main() {
  println(int.shift_right(1024, 3)) -- 128
  println(int.shift_right(65280, 8)) -- 255
  println(int.shift_right(-8, 1)) -- -4
}
```


## `int.to_radix`

```
int.to_radix(n: Int, radix: Int) -> String
```

Returns the text of `n` in `radix`, from 2 to 36: the digits `0`-`9` and then
lower-case letters, with a `-` in front of a negative number and no prefix.
`int.parse_radix` reads it back. A `radix` outside 2 to 36 is a runtime error.

```silt
import int

fn main() {
  println(int.to_radix(255, 16)) -- ff
  println(int.to_radix(-5, 2)) -- -101
  println(int.to_radix(35, 36)) -- z
}
```


## Int Constants

| Constant | Type | Value |
|----------|------|-------|
| `int.max_value` | `Int` | `9223372036854775807` |
| `int.min_value` | `Int` | `-9223372036854775808` |

The largest and the smallest `Int`, 2^63 - 1 and -2^63. Arithmetic that would
go past them is a runtime error (see
[Integer Overflow](../language/design-decisions.md#integer-overflow)).

```silt
import int

fn main() {
  println(int.max_value) -- 9223372036854775807
  println(int.min_value) -- -9223372036854775808
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
> range, for example with a `match` on the divisor that has an arm for
> `0.0`.

> **Note:** `round`, `ceil`, and `floor` return `Float`, not `Int`. Use
> `float.to_int` to convert the result to an integer.

> **How a float is shown:** `println`, `"{f}"` and `string.from(f)` write a
> `Float` as its digits with a decimal point and no exponent, so a whole one
> ends in `.0` (`3.0`, never `3`); `float.format(f, decimals)` writes exactly
> the decimal places asked for, which may be none.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `abs` | `(Float) -> Float` | Absolute value |
| `ceil` | `(Float) -> Float` | Round up to nearest integer (as Float) |
| `clamp` | `(Float, Float, Float) -> Float` | Clamp value to `[lo, hi]` |
| `floor` | `(Float) -> Float` | Round down to nearest integer (as Float) |
| `format` | `(Float, Int) -> String` | Decimal text with that many decimal places |
| `max` | `(Float, Float) -> Float` | Larger of two values |
| `min` | `(Float, Float) -> Float` | Smaller of two values |
| `parse` | `(String) -> Result(Float, FloatError)` | Parse string to float |
| `round` | `(Float) -> Float` | Round to nearest integer (as Float) |
| `to_int` | `(Float) -> Int` | Truncate to integer |
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
  println(float.abs(-3.14)) -- 3.14
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
  println(float.ceil(3.2)) -- 4.0
  println(float.ceil(-3.2)) -- -3.0
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
  println(float.clamp(0.5, 0.0, 1.0)) -- 0.5
  println(float.clamp(-0.2, 0.0, 1.0)) -- 0.0
  println(float.clamp(1.5, 0.0, 1.0)) -- 1.0
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
  println(float.floor(3.9)) -- 3.0
  println(float.floor(-3.2)) -- -4.0
}
```


## `float.format`

```
float.format(f: Float, decimals: Int) -> String
```

The decimal text of a float with exactly `decimals` decimal places, rounded
to the nearest; a number halfway between two is rounded away from zero, as
`float.round` rounds it (`2.5` is `3`, `-2.5` is `-3`). Halfway is judged
on the value the Float holds: `1.005` is held as a little less, so it is
`1.00` to two places. A number that rounds to zero is written without a sign.
`decimals` must not be negative. For the text a float is shown as (in
`println`, in `"{f}"`), use `string.from(f)`.

```silt
import float

fn main() {
  println(float.format(3.14159, 2)) -- 3.14
  println(float.format(42.0, 1)) -- 42.0
  println(float.format(2.5, 0)) -- 3
  println(float.format(0.125, 2)) -- 0.13
  println(float.format(-0.04, 1)) -- 0.0
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
  println(float.max(1.5, 2.5)) -- 2.5
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
  println(float.min(1.5, 2.5)) -- 1.5
}
```


## `float.parse`

```
float.parse(s: String) -> Result(Float, FloatError)
```

Parses a string as a float. Leading/trailing whitespace is trimmed. Returns
`Ok(f)` on success, `Err(FloatError)` on failure — an enum of its own with
the variants `int.parse` has (`Empty`, `InvalidDigit(offset)`, `Overflow`,
`Underflow`) and `NotFinite`. A `Float` is always finite, so
`"NaN"` and `"inf"` are rejected as `NotFinite` ("not a finite number"), and a
literal outside the `Float` range as `Overflow` (`"1e400"`, "number out of
range: above float.max_value") or `Underflow` (`"-1e400"`, "number out of
range: below float.min_value"). `InvalidDigit(offset)` is counted as for
`int.parse`: `"1.5.5"` is `InvalidDigit(3)`, `"1e"` (an exponent with no
digits) `InvalidDigit(2)`, and a lone sign `InvalidDigit(1)`.

```silt
import float

fn main() {
  match float.parse("3.14") {
    Ok(f) -> println(f)
    Err(e) -> println("error: {e}")
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
  println(float.round(3.6)) -- 4.0
  println(float.round(3.4)) -- 3.0
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
  println(float.to_int(3.9)) -- 3
  println(float.to_int(-3.9)) -- -3
}
```


## Float Constants

| Constant | Type | Value |
|----------|------|-------|
| `float.max_value` | `Float` | `1.7976931348623157e+308` |
| `float.min_value` | `Float` | `-1.7976931348623157e+308` |
| `float.epsilon` | `Float` | `2.220446049250313e-16` |
| `float.min_positive` | `Float` | `2.2250738585072014e-308` |
