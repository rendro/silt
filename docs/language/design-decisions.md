---
title: "Design Decisions"
section: "Language"
order: 10
---

# Design Trade-offs

## Interpolation Preferred for String Building

String `+` is supported (`p.first + " " + p.last`), but interpolation
`"{a}{b}"` is the preferred inline form: it reads more naturally for the
common case and keeps multi-fragment messages punctuation-light. For
pipelines, use `string.concat` or `string.join`.

## Homogeneous Maps

All map values must be the same type. For heterogeneous data, use records.
Heterogeneous maps would defeat the purpose of static typing.

## No Nested Named Functions

Named functions are top-level only. `let f = fn(x) { ... }` for local
helpers. Keeps scoping simple -- no hoisting, no forward-reference confusion.

## Pipe First-Argument Insertion

Matches Elixir convention. Simpler than auto-currying. Trade-off: no partial
application through pipes.

## `?` Precedence vs. `|>` and Arithmetic

`?` binds one step looser than `|>`, so `x |> f |> g?` parses as
`(x |> f |> g)?` — a trailing `?` applies to the whole pipeline without
parens. Infix arithmetic (`+`, `-`, `*`, `/`, `%`), `..`, and `as` all bind
tighter than `?`, so `x + y?` parses as `(x + y)?`. Comparison, boolean,
and `else` all bind looser, so `a == b?` is still `a == (b?)`.

## `fold_until` Same-Type Constraint

`Stop(value)` and `Continue(value)` carry the same accumulator type. For
search where the result type differs from state, use `loop`.

## Integer Overflow

Silt uses 64-bit signed integers. Arithmetic that overflows (e.g.
`9223372036854775807 + 1`) is a **runtime error**, not silent wrapping.
This matches the "explicit over implicit" philosophy -- silent wrong answers
are worse than crashes. `int.abs` also errors on the single unrepresentable
value: `let min = -9223372036854775807 - 1` constructs `Int::MIN` (since
the literal `9223372036854775808` is rejected at lex time), and
`int.abs(min)` then raises `integer overflow: abs(-9223372036854775808)`.

## Float Safety

Silt uses two float types: `Float` (guaranteed finite) and `ExtFloat` (full IEEE 754).
Division and functions that can produce NaN or Infinity return `ExtFloat`. The `else`
keyword narrows back to `Float` with an inline fallback:

```silt
let x: Float = 1.0 / 3.0 else 0.0       -- finite result -> 0.333...
let y: Float = 1.0 / 0.0 else 0.0       -- infinity -> fallback 0.0
let z: Float = math.sqrt(-1.0) else 0.0  -- NaN -> fallback 0.0
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

Postfix operators (function call, `?`, trailing closure) do **not**
cross newlines. Infix operators (`|>`, `.`, `==`, `*`, etc.) do. `+` and `-`
are ambiguous (also unary) so they do not cross newlines -- place them at the
end of the line to continue:

```silt
let x = 10 +
  20            -- OK: + at end of line

let y = 10
  + 20          -- NOT a continuation
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
