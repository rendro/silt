---
title: "math"
section: "Standard Library"
order: 11
---

# math

Mathematical functions and constants. Functions that always produce finite results from
finite inputs return `Float`. Functions that may produce NaN or Infinity return `ExtFloat`
— use `else` to narrow back to `Float`.

## Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `acos` | `(Float) -> ExtFloat` | Arccosine (radians) |
| `asin` | `(Float) -> ExtFloat` | Arcsine (radians) |
| `atan` | `(Float) -> Float` | Arctangent (radians) |
| `atan2` | `(Float, Float) -> Float` | Two-argument arctangent |
| `cos` | `(Float) -> Float` | Cosine |
| `e` | `Float` | Euler's number (2.71828...) |
| `exp` | `(Float) -> ExtFloat` | Exponential (e^x) |
| `log` | `(Float) -> ExtFloat` | Natural logarithm (ln) |
| `log10` | `(Float) -> ExtFloat` | Base-10 logarithm |
| `pi` | `Float` | Pi (3.14159...) |
| `pow` | `(Float, Float) -> ExtFloat` | Exponentiation |
| `sin` | `(Float) -> Float` | Sine |
| `sqrt` | `(Float) -> ExtFloat` | Square root |
| `tan` | `(Float) -> Float` | Tangent |


## `math.acos`

```
math.acos(x: Float) -> ExtFloat
```

Returns the arccosine of `x` in radians. Returns `NaN` for inputs outside [-1, 1].
Use `else` to narrow:

```silt
fn main() {
    let angle = math.acos(1.0) else 0.0
    println(angle)  // 0.0
}
```


## `math.asin`

```
math.asin(x: Float) -> ExtFloat
```

Returns the arcsine of `x` in radians. Returns `NaN` for inputs outside [-1, 1].
Use `else` to narrow:

```silt
fn main() {
    let angle = math.asin(1.0) else 0.0
    println(angle)  // 1.5707... (pi/2)
}
```


## `math.atan`

```
math.atan(x: Float) -> Float
```

Returns the arctangent of `x` in radians.

```silt
fn main() {
    println(math.atan(1.0))  // 0.7853... (pi/4)
}
```


## `math.atan2`

```
math.atan2(y: Float, x: Float) -> Float
```

Returns the angle in radians between the positive x-axis and the point (x, y).
Handles all quadrants correctly.

```silt
fn main() {
    println(math.atan2(1.0, 1.0))  // 0.7853... (pi/4)
}
```


## `math.cos`

```
math.cos(x: Float) -> Float
```

Returns the cosine of `x` (in radians).

```silt
fn main() {
    println(math.cos(0.0))       // 1.0
    println(math.cos(math.pi))   // -1.0
}
```


## `math.e`

```
math.e : Float
```

Euler's number, approximately 2.718281828459045. This is a constant, not a
function.

```silt
fn main() {
    println(math.e)  // 2.718281828459045
}
```


## `math.exp`

```
math.exp(x: Float) -> ExtFloat
```

Returns e raised to the power of `x`. May overflow to Infinity for large inputs.
Use `else` to narrow:

```silt
fn main() {
    let result = math.exp(1.0) else 0.0
    println(result)  // 2.718281828459045
}
```


## `math.log`

```
math.log(x: Float) -> ExtFloat
```

Returns the natural logarithm (base e) of `x`. Returns `-Infinity` for zero,
`NaN` for negative inputs. Use `else` to narrow:

```silt
fn main() {
    let result = math.log(math.e) else 0.0
    println(result)  // 1.0
}
```


## `math.log10`

```
math.log10(x: Float) -> ExtFloat
```

Returns the base-10 logarithm of `x`. Returns `-Infinity` for zero,
`NaN` for negative inputs. Use `else` to narrow:

```silt
fn main() {
    let result = math.log10(100.0) else 0.0
    println(result)  // 2.0
}
```


## `math.pi`

```
math.pi : Float
```

Pi, approximately 3.141592653589793. This is a constant, not a function.

```silt
fn main() {
    let circumference = 2.0 * math.pi * 5.0
    println(circumference)
}
```


## `math.pow`

```
math.pow(base: Float, exponent: Float) -> ExtFloat
```

Returns `base` raised to the power of `exponent`. Returns `ExtFloat` — may be
Infinity for large results. Use `else` to narrow:

```silt
fn main() {
    let result = math.pow(2.0, 10.0) else 0.0
    println(result)  // 1024.0
}
```


## `math.sin`

```
math.sin(x: Float) -> Float
```

Returns the sine of `x` (in radians).

```silt
fn main() {
    println(math.sin(0.0))           // 0.0
    println(math.sin(math.pi / 2.0)) // 1.0
}
```


## `math.sqrt`

```
math.sqrt(x: Float) -> ExtFloat
```

Returns the square root of `x`. Returns `NaN` for negative inputs. Use `else`
to narrow:

```silt
fn main() {
    let result = math.sqrt(4.0) else 0.0
    println(result)  // 2.0
}
```


## `math.tan`

```
math.tan(x: Float) -> Float
```

Returns the tangent of `x` (in radians).

```silt
fn main() {
    println(math.tan(0.0))           // 0.0
    println(math.tan(math.pi / 4.0)) // 1.0 (approximately)
}
```
