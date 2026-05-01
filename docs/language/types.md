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

## Numeric Safety

silt treats silent wrong answers as worse than crashes. The numeric types are designed so that every value in `Int` and `Float` is a finite, ordinary number.

### Integer overflow

`Int` is 64-bit signed. Arithmetic that would overflow is a **runtime error**, not silent wrapping:

```silt
9223372036854775807 + 1      -- runtime error: integer overflow
let min = -9223372036854775807 - 1   -- Int::MIN, the unrepresentable value
int.abs(min)                  -- runtime error: integer overflow: abs(-9223372036854775808)
```

The lexer rejects `9223372036854775808` directly as a number literal
(it overflows `Int`); to construct `Int::MIN` you write
`-9223372036854775807 - 1` because unary `-` is a separate operator,
not part of the literal.

### Finite floats and `ExtFloat`

Operations that *can* produce `NaN` or `Infinity` return `ExtFloat` instead of `Float`. This splits the type system: `Float` values are always finite and totally ordered, `ExtFloat` values may be anything IEEE 754 produces.

```silt
1.0 + 2.0        -- Float        (addition of finite Floats)
1.0 / 2.0        -- ExtFloat     (division may produce Infinity)
math.sqrt(x)     -- ExtFloat     (may produce NaN)
```

Non-division arithmetic (`+`, `-`, `*`) on `Float` values stays in `Float` and panics on overflow to `Infinity`, matching the integer rule.

### Recovering `Float` with `else`

To use an `ExtFloat` where a `Float` is needed, supply a finite fallback with the `else` operator:

```silt
let x: Float = 1.0 / 3.0 else 0.0        -- finite result → 0.333...
let y: Float = 1.0 / 0.0 else 0.0        -- infinity → fallback 0.0
let z: Float = math.sqrt(-1.0) else 0.0  -- NaN → fallback 0.0
```

`else` is the lowest-precedence infix operator. See [Operators and Precedence](operators.md) for details.

### No implicit coercion

There are no implicit conversions between `Int` and `Float`. Convert explicitly with `int.to_float(n)` and `float.to_int(x)`. The `Float` → `ExtFloat` direction is safe (every finite value is valid IEEE 754) and happens automatically where needed; the `ExtFloat` → `Float` direction always requires `else`.

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

## Anonymous Records and Row Polymorphism

Beside nominal records (`type User { ... }`), silt has **anonymous
structural records**: a record literal or type written without a name.
Two anonymous records with the same fields and field types are the same
type — there is no `type Foo { ... }` declaration to anchor identity.

```silt
let alice = { name: "Alice", age: 30 }
let bob   = { name: "Bob",   age: 25 }   -- same type as alice
alice.name   -- "Alice"
```

The type of `alice` is the structural type `{name: String, age: Int}`,
which can also appear in any annotation:

```silt
fn full_name(p: {first: String, last: String}) -> String =
  p.first + " " + p.last
```

### Open rows: `...r`

A record type can leave its tail **open** with `...r`, where `r` is a
lowercase row variable — a polymorphic placeholder for "any further
fields you happen to have." The function only commits to the fields it
names; the row variable absorbs whatever else the caller passes:

```silt
fn first_name(p: {name: String, ...r}) -> String = p.name

fn main() {
  println(first_name({name: "Alice", age: 30}))
  println(first_name({name: "Bob"}))
}
```

Inside the function `p.name` is the only legal access; `p.age` would be
rejected, even when the caller passes a record that happens to have an
`age` field.

A row variable can be threaded into the return type so the caller's
extra fields survive the round trip:

```silt
fn id_name(p: {name: String, ...r}) -> {name: String, ...r} = p

fn main() {
  let q = id_name({name: "Alice", age: 30})
  -- `age` came along with the row
  println(q.age)       -- 30
}
```

Row variables are inferred at first appearance, just like ordinary type
variables: a function written without annotations like
`fn show_name(p) = p.name` is inferred to take an open record carrying
at least a `name` field.

### Nominal records flow into open rows

Nominal records widen to open rows automatically, so a fn taking a
`{name: String, ...r}` parameter accepts any nominal record that has a
`name: String` field:

```silt
type Person { name: String, age: Int }

fn name(p: {name: String, ...r}) -> String = p.name

fn main() {
  println(name(Person { name: "Bob", age: 42 }))   -- Bob
}
```

The reverse direction does not happen — anonymous records do not
automatically become nominal. Use the constructor (`Person { ... }`)
when nominal identity matters.

### Closed rows reject extra fields

A record type **without** `...r` is closed: the caller must supply
exactly the listed fields, no more, no less:

```silt
fn ident(x: {a: Int, b: String}) -> {a: Int, b: String} = x

ident({a: 1, b: "hi"})            -- OK
ident({a: 1, b: "hi", c: 9})      -- error: extra field `c`
ident({a: 1})                     -- error: missing field `b`
```

### Record extension: `{...p, ...}`

The same `...` token, in a record **expression**, spreads an existing
record into a new one. Additional fields after the spread are appended;
attempting to overwrite a field that already exists is rejected:

```silt
fn main() {
  let p = {name: "Alice"}
  let q = {...p, age: 30}        -- {name: String, age: Int}
  println(q.name)                -- Alice
  println(q.age)                 -- 30
}
```

Trying to redefine an existing field — `{...p, name: "Bob"}` — is a
compile-time error. The shape is "extend, never overwrite"; use record
update (`p.{ name: "Bob" }`) for that.

### Pattern destructuring with rest

Record patterns mirror the type form. A `{name: nm, ...rest}` pattern
in a `match` arm binds the listed fields and lets a row variable
capture the rest of the type, the same way `..rest` works on lists:

```silt
fn main() {
  let p = {name: "A", age: 30}
  match p {
    {name: nm} -> println(nm)      -- "A"
  }
}
```

See [pattern matching](pattern-matching.md) for the full record-pattern
grammar.

### Summary

| Form                              | Meaning                                                |
|-----------------------------------|--------------------------------------------------------|
| `{name: "A", age: 30}`            | Anonymous record literal                               |
| `{name: String, age: Int}`        | Closed structural record type — exactly these fields   |
| `{name: String, ...r}`            | Open row — at least `name`, plus any tail captured by `r` |
| `{...p, age: 30}`                 | Record extension — copy `p`, append `age` (no overwrite) |

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
let x = [] as List(Int)
let r = (int.parse("42") as Result(Int, ParseError))?
```

`as` is a compile-time assertion — if the types conflict, you get a type error.
At runtime it's a no-op.
