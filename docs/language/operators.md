---
title: "Operators and Precedence"
section: "Language"
order: 12
description: "Complete operator reference: symbols, precedence, associativity, and newline-sensitivity rules."
---

# Operators and Precedence

This page is the authoritative reference for every operator silt recognises, what it does, how tightly it binds, and which operators are newline-sensitive.

## Operator Table

Operators are listed from **lowest precedence** (binds loosest) to **highest precedence** (binds tightest). Every infix operator is left-associative.

| Precedence | Operator        | Kind           | Meaning                                            |
|-----------:|-----------------|----------------|----------------------------------------------------|
|         10 | `else`          | infix          | `ExtFloat else Float` → `Float` fallback           |
|         20 | `\|\|`          | infix          | Boolean OR (short-circuiting)                      |
|         30 | `&&`            | infix          | Boolean AND (short-circuiting)                     |
|         40 | `==`, `!=`      | infix          | Equality / inequality                              |
|         50 | `<`, `>`, `<=`, `>=` | infix     | Ordered comparison                                 |
|         54 | `?`             | **postfix**    | Error propagation (`Result` / `Option`)            |
|         55 | `\|>`           | infix          | Pipe: `x \|> f` = `f(x)`                           |
|         60 | `..`            | infix          | Inclusive range                                    |
|         70 | `+`, `-`        | infix          | Addition, subtraction (newline-sensitive)          |
|         80 | `*`, `/`, `%`   | infix          | Multiplication, division, modulo                   |
|         90 | `-x`, `!x`      | **prefix**     | Numeric negation, boolean NOT                      |
|         95 | `as`            | infix          | Type ascription: `expr as Type`                    |
|        115 | `{ ... }`       | postfix        | Trailing closure (only on same line as call)       |
|        120 | `f(...)`        | postfix        | Function call                                      |
|        130 | `.`             | infix/postfix  | Field access, `expr.{ ... }` record update         |

`?` is postfix and sits between comparison (`<`, `>=`) and pipe (`|>`).

silt has **no postfix bracket indexing** (`xs[i]`). The parser rejects it
with `postfix indexing is not supported; use list.get(xs, i), map.get(m, k), or string.slice(s, i, i + 1)`.
Use the explicit module function for the collection you have:
`list.get(xs, i)`, `map.get(m, k)`, `string.slice(s, i, i + 1)`.

## Reading the Table

Higher precedence wins. Given `a + b * c`, `*` (80) binds tighter than `+` (70), so the expression parses as `a + (b * c)`. All infix operators are left-associative, so `a - b - c` parses as `(a - b) - c`.

Unary `-` and `!` have precedence 90 — tighter than `*`, looser than `as`. So `-x * y` is `(-x) * y`, and `-x as ExtFloat` parses as `-(x as ExtFloat)`.

## Error Propagation (`?`)

`?` is a postfix operator: `expr?`. It unwraps `Result` or `Option`, propagating `Err` / `None` out of the surrounding function.

Key precedence consequences:

```silt
x |> f |> g?        -- (x |> f |> g)?   -- ? applies to the whole pipeline
x + y?              -- (x + y)?          -- arithmetic binds tighter than ?
a == b?             -- a == (b?)         -- comparison binds looser than ?
1..10?              -- (1..10)?          -- range binds tighter than ?
```

This is deliberate: the shape `pipeline?` is common and should not require parentheses, and the shapes `(x + y)?` and `(1..10)?` are type errors on non-`Result` operands anyway, so moving `?` outward does not change valid programs.

See [Error Handling](error-handling.md) for the full semantics.

## Pipe (`|>`)

`|>` inserts the left value as the **first argument** of the call on the right:

```silt
-- these are equivalent:
list.map(xs, { n -> n * 2 })
xs |> list.map { n -> n * 2 }
```

Pipe binds tighter than comparison and boolean operators, so `x |> f == y` parses as `(x |> f) == y`. It binds looser than range, so `1..10 |> list.sum()` works without parentheses.

## Float Recovery (`else`)

`else` is the lowest-precedence infix operator. It narrows `ExtFloat` (IEEE 754) to `Float` (guaranteed finite) by supplying a fallback for `NaN` / `Infinity`:

```silt
let x: Float = 1.0 / 3.0 else 0.0       -- finite → 0.333...
let y: Float = 1.0 / 0.0 else 0.0       -- infinity → fallback 0.0
let z: Float = math.sqrt(-1.0) else 0.0 -- NaN → fallback 0.0
```

See [Types](types.md#numeric-safety) for when `ExtFloat` arises.

## Newline Sensitivity

silt has no statement separator. Newlines can end an expression, but the rules depend on the operator:

**Infix operators cross newlines:**

```silt
let total = a
  * b             -- OK: * never unary, unambiguous
  * c

items
  |> list.filter { n -> n > 0 }
  |> list.map { n -> n * n }
```

**`+` and `-` do not cross newlines** (they are ambiguous with unary `-x` / `+x`):

```silt
let x = 10 +
  20              -- OK: + at end of line

let y = 10
  + 20            -- NOT a continuation — `y = 10` then `+20` starts a new expr
```

**Postfix operators do not cross newlines.** Call, `?`, and trailing closure must appear on the same line as their operand:

```silt
let n = parse(input)?       -- OK

let n = parse(input)
  ?                         -- NOT a ?-propagation; parses as two expressions
```

```silt
xs |> list.map { x -> x + 1 }    -- OK

xs |> list.map
  { x -> x + 1 }                 -- NOT a trailing closure
```

Put the continuation operator at the **end** of the previous line, not the start of the next.

## Unary Operators

| Operator | Applies to | Example              |
|----------|-----------|----------------------|
| `-x`     | numeric   | `-42`, `-x * y`      |
| `!x`     | `Bool`    | `!done`, `!(a == b)` |

Both have precedence 90 — tighter than any binary arithmetic, looser than `as`, call, or field access.

## Range (`..`)

`a..b` is an inclusive range from `a` to `b`. It has type `Range(Int)`, a
nominal wrapper that converts implicitly to and from `List(Int)`, so ranges
work anywhere a list does:

```silt
1..100 |> list.sum()              -- 5050
(1..n) |> list.each { i -> ... }
let r: Range(Int) = 1..10         -- annotated
let xs: List(Int) = 1..10         -- implicit Range→List
```

Ranges are lazy — they don't allocate memory until iterated, so
`1..1000000` is cheap. `Range` is a distinct runtime variant
(`Value::Range(lo, hi)`), not a materialized list, and the iteration
helpers in `list.*` walk it without first expanding it into a `List(Int)`.

Range binds tighter than `|>` so `1..10 |> list.sum()` needs no parens, and looser than arithmetic so `a+1..b-1` works.

## Type Ascription (`as`)

`expr as Type` constrains the type of an expression. Used mainly to
disambiguate polymorphic literals where inference cannot pick a concrete
type from context:

```silt
let xs = [] as List(Int)
let rows = [[]] as List(List(Int))
```

`as` is a compile-time *assertion*, not a coercion: it only succeeds when
the expression already has the named type. There is no implicit numeric
conversion — `42 as Float` is rejected with `type mismatch: expected
Float, got Int`. Convert numbers explicitly with `int.to_float(42)` or
`float.to_int(x)`.

## Field Access and Record Update (`.`)

`.` has the highest precedence of any operator. It is used for:

- **Field access:** `user.name`
- **Record update:** `user.{ age: 31 }` produces a new record with `age` replaced

Tuples have no postfix index operator — destructure them with a `match`
pattern instead, e.g. `match pair { (a, b) -> ... }`. See
[Pattern Matching](pattern-matching.md#tuple-patterns).

```silt
let bob = User { name: "bob", age: 30 }
let older = bob.{ age: bob.age + 1 }
```

## See Also

- [Bindings and Functions](bindings-and-functions.md) — where operators appear in expression position
- [Pattern Matching](pattern-matching.md) — guard expressions use the same operators
- [Error Handling](error-handling.md) — full `?` semantics and `Result` / `Option`
- [Types](types.md) — `Float` vs `ExtFloat` and the `else` operator
- [Design Decisions](design-decisions.md) — rationale for `?` precedence and overflow behaviour
