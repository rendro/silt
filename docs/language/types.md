---
title: "Types"
---

# Types

## Primitive Types

| Type     | Description                       | Examples                  |
|----------|-----------------------------------|---------------------------|
| `Int`    | 64-bit signed integer (overflow is a runtime error) | `42`, `-7`, `0xFF`, `0b1010` |
| `Float`  | 64-bit floating-point, guaranteed finite | `3.14`, `-0.5`, `1e5`, `2.5e-3` |
| `ExtFloat` | 64-bit floating-point (IEEE 754, allows NaN/Infinity) | Division and some math results |
| `Bool`   | Boolean                           | `true`, `false`           |
| `String` | UTF-8 string with interpolation   | `"hello"`, `"age: {n}"`  |
| `Unit`   | No meaningful value               | (returned by `println`)   |

No implicit conversions. Use `int.to_float()` or `float.to_int()` explicitly.

### Numeric Literals

All numeric literals support `_` as a visual separator: `1_000_000`, `0xFF_FF`.

```silt
-- Decimal
let n = 42
let big = 1_000_000

-- Hex and binary (always Int)
let mask = 0xFF
let flags = 0b1010_0001

-- Scientific notation (always Float)
let avogadro = 6.022e23
let tiny = 1e-9
let hundred = 1e2       -- Float(100.0), not Int
```

Scientific notation always produces a `Float`, even when the value is a whole number.
Non-finite results like `1e999` are rejected at compile time.

## Enums (Tagged Unions)

```silt
type Shape {
  Circle(Float)
  Rect(Float, Float)
}

type Color { Red, Green, Blue }
```

Constructors create values: `Circle(5.0)`, `Rect(3.0, 4.0)`. The compiler
checks exhaustiveness when you match on them.

## Generic Types

```silt
type Option(a) { Some(a), None }
type Result(a, e) { Ok(a), Err(e) }
```

Type parameters are filled in at use: `Option(Int)`, `Result(String, String)`.

## Records

```silt
type User {
  name: String,
  age: Int,
  active: Bool,
}

let alice = User { name: "Alice", age: 30, active: true }
alice.name   -- "Alice"
```

**Record update syntax** creates a new record with fields changed:

```silt
let alice2 = alice.{ age: 31 }
```

Read as "alice, but with age 31." Compare to Elm `{ u | age = 31 }`, Rust
`User { age: 31, ..u }`. Silt's `.{ }` syntax avoids new keywords or sigils.

## Tuples

Fixed-size, heterogeneous:

```silt
let pair = (1, "hello")
let (x, y) = pair
```

## Recursive Types

Types can reference themselves:

```silt
type Expr {
  Num(Int)
  Add(Expr, Expr)
}
```

## Function Type Annotations

```silt
let apply: Fn(Int, Int) -> Int = fn(a, b) { a + b }

type Handler {
  name: String,
  run: Fn(String) -> String,
}
```

## Type Ascription

When type inference cannot determine a type from context, use `as` to assert it:

```silt
let x = empty() as List(Int)
let r = (parse("42") as Result(Int, String))?
```

`as` is a compile-time assertion — if the types conflict, you get a type error.
At runtime it's a no-op.
