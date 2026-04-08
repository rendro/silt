---
title: "Design Decisions"
---

# Design Trade-offs

## No String Concatenation Operator

Interpolation `"{a}{b}"` is the only inline way. Eliminates `"hello " + name
+ "!"` patterns. For pipelines, use `string.join`.

## Homogeneous Maps

All map values must be the same type. For heterogeneous data, use records.
Heterogeneous maps would defeat the purpose of static typing.

## No Nested Named Functions

Named functions are top-level only. `let f = fn(x) { ... }` for local
helpers. Keeps scoping simple -- no hoisting, no forward-reference confusion.

## Pipe First-Argument Insertion

Matches Elixir convention. Simpler than auto-currying. Trade-off: no partial
application through pipes.

## `fold_until` Same-Type Constraint

`Stop(value)` and `Continue(value)` carry the same accumulator type. For
search where the result type differs from state, use `loop`.

## Integer Overflow

Silt uses 64-bit signed integers. Arithmetic that overflows (e.g.
`9223372036854775807 + 1`) is a **runtime error**, not silent wrapping.
This matches the "explicit over implicit" philosophy -- silent wrong answers
are worse than crashes. `int.abs` also errors on the single unrepresentable
value (`int.abs(-9223372036854775808)`).

## Float Safety

Silt uses two float types: `Float` (guaranteed finite) and `ExtFloat` (full IEEE 754).
Division and functions that can produce NaN or Infinity return `ExtFloat`. The `else`
keyword narrows back to `Float` with an inline fallback:

```silt
let x: Float = 1.0 / 3.0 else 0.0       // finite result → 0.333...
let y: Float = 1.0 / 0.0 else 0.0       // infinity → fallback 0.0
let z: Float = math.sqrt(-1.0) else 0.0  // NaN → fallback 0.0
```

Non-division arithmetic (`+`, `-`, `*`) on `Float` values still returns `Float` and
panics on overflow to Infinity, matching the integer overflow philosophy.

## No Negative Indexing

`list.get(xs, -1)` is a runtime error, not "last element." Indices are
positions from the start, period. Use `list.last(xs)` for the last element,
or `list.get(xs, list.length(xs) - 1)` for explicit end-relative access.
This keeps the mental model simple and avoids hidden "if negative, wrap"
logic.

## Immutability Cost

DP and graph algorithms must thread state through `loop` or `fold`. More
verbose, but enables concurrency safety and reasoning guarantees.

## Newline Sensitivity

Postfix operators (function call, `?`, trailing closure, index) do **not**
cross newlines. Infix operators (`|>`, `.`, `==`, `*`, etc.) do. `+` and `-`
are ambiguous (also unary) so they do not cross newlines -- place them at the
end of the line to continue:

```silt
let x = 10 +
  20            -- OK: + at end of line

let y = 10
  + 20          -- NOT a continuation
```

Trailing closures must start on the same line as the function call:

```silt
xs |> list.map { x -> x + 1 }       -- OK
xs |> list.map { x ->                -- OK: { on same line
  x + 1
}
```
