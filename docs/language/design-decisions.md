---
title: "Design Decisions"
section: "Language"
order: 10
---

# Design Trade-offs

## Interpolation for String Building

`+` is arithmetic only. Strings are built with interpolation
(`"{p.first} {p.last}"`), which reads naturally and keeps multi-fragment
messages punctuation-light. For a list of strings, use `string.join`.

## Homogeneous Maps

All map values must be the same type. For heterogeneous data, use records.
Heterogeneous maps would defeat the purpose of static typing.

## No Nested Named Functions

Named functions are top-level only. `let f = { x -> ... }` for local
helpers. Keeps scoping simple -- no hoisting, no forward-reference confusion.

## Pipe First-Argument Insertion

Matches Elixir convention. Simpler than auto-currying. Trade-off: no partial
application through pipes.

## `?` Precedence

`?` is a tight postfix operator: it binds like a call, so
`int.parse(a)? + int.parse(b)?` adds two unwrapped values and `-x?` negates
the unwrapped `x`. One rule covers pipelines: a `?` that ends a pipeline
applies to the whole pipeline, so `x |> f |> g?` parses as
`(x |> f |> g)?` — the common shape `pipeline?` needs no parentheses.

## `fold_until` Same-Type Constraint

`Stop(value)` and `Continue(value)` carry the same accumulator type. For
search where the result type differs from state, use `loop`.

## Integer Overflow

Silt uses 64-bit signed integers. Arithmetic that overflows (e.g.
`9223372036854775807 + 1`) is a **runtime error**, not silent wrapping.
This matches the "explicit over implicit" philosophy -- silent wrong answers
are worse than crashes. `int.abs` also errors on the single unrepresentable
value: `int.abs(-9223372036854775808)` raises
`integer overflow: abs(-9223372036854775808)`.

## Float Safety

Silt has one float type, `Float`, and a `Float` is always finite: never NaN,
never infinite. An operation whose result would be NaN or infinite is a
**runtime error**, the same rule as integer overflow:

```silt
1.0 / 0.0                -- error: float division by zero
float.max_value * 2.0    -- error: float overflow
math.sqrt(-4.0)          -- error: math.sqrt of a negative number: -4
math.log(0.0)            -- error: math.log of a number that is not positive: 0
```

Where an input can be out of range, guard it explicitly:

```silt
let ratio = match total {
  0.0 -> 0.0
  _ -> part / total
}
```

Because every `Float` is finite, `==`, ordering, hashing, sets and maps all
treat floats as ordinary totally ordered values, and `-0.0` is the same value
as `0.0` (`0.0 * -1.0` prints `0`).

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

Postfix operators (function call, `?`, trailing closure) do **not**
cross newlines. Infix operators (`|>`, `.`, `==`, `*`, etc.) do. `+` and `-`
do not cross newlines -- `-` is ambiguous with unary negation, and `+` is
treated symmetrically even though silt has no unary plus -- place them at
the end of the line to continue:

```silt
let x = 10 +
  20            -- OK: + at end of line

let y = 10
  + 20          -- NOT a continuation -- parse error (no unary plus)
```

(Bracket indexing `xs[i]` is reserved syntax but is not a real postfix
operator -- silt's parser rejects it. Use `list.get(xs, i)`,
`map.get(m, k)`, or `string.slice(s, i, i + 1)` instead.)

Trailing closures must start on the same line as the function call:

```silt
xs |> list.map { x -> x + 1 }       -- OK
xs |> list.map { x ->                -- OK: { on same line
  x + 1
}
```
