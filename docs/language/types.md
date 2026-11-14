---
title: "Types"
section: "Language"
order: 2
---

# Types

## Primitive Types

| Type     | Description                       | Examples                  |
|----------|-----------------------------------|---------------------------|
| `Int`    | 64-bit signed integer (overflow is a runtime error) | `42`, `-7`, `0xFF`, `0b1010` |
| `Float`  | 64-bit floating-point, guaranteed finite | `3.14`, `-0.5`, `1e5`, `2.5e-3` |
| `Bool`   | Boolean                           | `true`, `false`           |
| `String` | UTF-8 string with interpolation   | `"hello"`, `"age: {n}"`  |
| `()`     | The unit type: no meaningful value | `()` (returned by `println`) |

No implicit conversions. Use `Float.from_int()` or `float.to_int()` explicitly.

### Numeric Literals

A `_` between two digits is a visual separator: `1_000_000`, `0xFF_FF`.

A number has one spelling. The prefixes `0x` and `0b` and the exponent
`e` are written in lower case (the digits of a hex number may be of
either case), an exponent has no `+`, a `_` stands only between two
digits, and a decimal number has no leading zero: `7`, not `007`. Any
other spelling is an error that writes the number in its one spelling.

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
let hundred = 1e2 -- Float(100.0), not Int
```

Scientific notation always produces a `Float`, even when the value is a whole number.
A `Float` is always printed with a decimal point and without an exponent, so a whole one ends in `.0`: `println(1e2)` prints `100.0`.
Non-finite results like `1e999` are rejected at compile time.

## Numeric Safety

silt treats silent wrong answers as worse than crashes. The numeric types are designed so that every value in `Int` and `Float` is a finite, ordinary number.

### Integer overflow

`Int` is 64-bit signed. Arithmetic that would overflow is a **runtime error**, not silent wrapping:

```silt
println(9223372036854775807 + 1) -- runtime error: integer overflow
let min = -9223372036854775808 -- the smallest Int
println(int.abs(min)) -- runtime error: integer overflow: abs(-9223372036854775808)
```

The smallest `Int` is written `-9223372036854775808`: the minus sign and
the digits are one literal there. Without the minus sign,
`9223372036854775808` is one more than the largest `Int` and is rejected
(`number literal too large`).

### Finite floats

A `Float` is always finite: never `NaN`, never infinite. An operation whose result would be `NaN` or infinite is a **runtime error** that names the operation, matching the integer rule:

```silt
println(1.0 / 2.0) -- 0.5
println(1.0 / 0.0) -- runtime error: float division by zero
println(float.max_value * 2.0) -- runtime error: float overflow
println(math.sqrt(-4.0)) -- runtime error: math.sqrt of a negative number: -4.0
```

The same holds for every function that returns a `Float`: `math.log` of a number that is not positive, `math.asin` / `math.acos` outside -1..1, `math.pow` and `math.exp` overflow, and `list.sum` / `list.product` of `Float`s overflow all raise. `float.parse` returns an `Err` for `"inf"`, `"NaN"` and out-of-range literals, and decoding a non-finite number into a `Float` field (`toml.parse`) is an error.

Where an input can be out of range, guard it explicitly:

```silt
let ratio = match total {
  0.0 -> 0.0
  _ -> part / total
}
```

Because every `Float` is finite, floats compare, sort and hash as ordinary numbers and work as map keys and set elements. `-0.0` is the same value as `0.0`: `0.0 * -1.0` prints `0.0`.

### No implicit coercion

There are no implicit conversions between `Int` and `Float`. Convert explicitly with `Float.from_int(n)` and `float.to_int(x)`.

## Enums (Tagged Unions)

```silt
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

type Color {
  Red,
  Green,
  Blue,
}
```

Constructors create values: `Circle(5.0)`, `Rect(3.0, 4.0)`. The compiler
checks exhaustiveness when you match on them.

## Generic Types

```silt
type Option(a) {
  Some(a),
  None,
}

type Result(a, e) {
  Ok(a),
  Err(e),
}
```

Type parameters are filled in at use: `Option(Int)`, `Result(String, String)`.

## Records

```silt
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn main() {
  let alice = User { name: "Alice", age: 30, active: true }
  println(alice.name) -- Alice
}
```

A field written alone takes the value of the name in scope: `User { name,
age, active: true }` is `User { name: name, age: age, active: true }`,
as the record pattern `User { name, age, .. }` binds `name` and `age`.
Building and reading a record look alike. A record literal starts with
the name of its type: braces alone, `{ name }`, are a block, and
`{ name: "A" }` is an error.

A field is read on a value whose type is known where it is read, or
later in the same function. A function that reads a field of a
parameter names the parameter's type,
`fn age(u: User) -> Int { u.age }`: a field of a value whose type
nothing in the function decides is an error that asks for the
annotation.

In the header of a `match` or a `loop`, braces that hold one name alone
are the header's block, as `match x { y }` would be: there a literal
with a single field written alone goes in parentheses,
`match (User { name }) { ... }`, or writes the field out,
`match User { name: name } { ... }`.

Records declared in another module can be constructed with a
**module-qualified path** — no selective import needed. The qualifier is
explicit naming, not a second construction form: `users.User { ... }`
builds exactly the same value as `User { ... }` does after
`import users.{ User }`, and it picks the right declaration when two
imported modules export the same type name.

```silt
-- noexec (multi-file: users.silt declares `pub type User { ... }`)
import users

let alice = users.User { name: "Alice", age: 30, active: true }
```

**Record update syntax** creates a new record with fields changed:

```silt
let alice2 = alice.{ age: 31 }
```

The shorthand works here too: `alice.{ age }` sets `age` to the value of
the name `age`.

A keyword may name a field, where only a field name can stand: in the
record type's body, before `:` in a literal or an update, in a record
pattern and after a dot. So a JSON document's `"type"` key has a field
of its name:

```silt
type Event {
  type: String,
  id: Int,
}

fn main() {
  let e = Event { type: "click", id: 1 }
  match e {
    Event { type: t, .. } -> println(t) -- click
  }
  println(e.type) -- click
}
```

Such a field is never written alone (`Event { type }`): the shorthand
binds or reads a variable, and a keyword names none.

Read as "alice, but with age 31." Compare to Elm `{ u | age = 31 }`, Rust
`User { age: 31, ..u }`. Silt's `.{ }` syntax avoids new keywords or sigils.

## Type Aliases

`type Name = T` gives the type `T` another name; the name stands for
`T` wherever it is written. An alias of a record type builds and
matches that record (`type Q = P`, then `Q { x: 1 }` and `Q { x } ->`),
an alias of an enum writes its variants (`type C = Color`, then `C.Red`),
an alias with arguments gives them (`type IntBox = Box(Int)`, then
`IntBox { value: 1 }` is a `Box(Int)`), and an alias is a type argument
as its type is (`json.parse(text, Q)`).

## Tuples

Fixed-size, heterogeneous:

```silt
let pair = (1, "hello")
let (x, y) = pair
```

Parentheses around one type or one expression only group: `(Int)` is `Int`
and `(1)` is `1`. A tuple of one element is written with a comma, `(Int,)`
and `(1,)`; `()` is the unit type and its value.

## Recursive Types

Types can reference themselves:

```silt
type Expr {
  Num(Int),
  Add(Expr, Expr),
}
```

## Function Type Annotations

```silt
let apply: Fn(Int, Int) -> Int = { a, b -> a + b }

type Handler {
  name: String,
  run: Fn(String) -> String,
}
```

## Type Ascription

When type inference cannot determine a type from context, use `as` to assert it:

```silt
let x = [] as List(Int)
let r = (int.parse("42") as Result(Int, int.IntError))?
```

`as` is a compile-time assertion — if the types conflict, you get a type error.
At runtime it's a no-op.
