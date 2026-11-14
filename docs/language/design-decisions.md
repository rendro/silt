---
title: "Design Decisions"
section: "Language"
order: 11
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
A local closure can still call itself by the name its `let` gives it
(`let f = { n -> ... f(n - 1) ... }`), as a top-level one can; nothing
later than the `let` is visible in it.

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

Silt uses 64-bit signed integers, and arithmetic that overflows is a
**runtime error**, not silent wrapping. This matches the "explicit over
implicit" philosophy: a silent wrong answer is worse than a crash. The rule
and its messages are in [Types: Integer overflow](types.md#integer-overflow).

## What `main` Returns

A program's `main`, and a test function, returns `()` or `Result((), e)`:
nothing else. A program ends well or it fails, and the shell sees exactly
that: status 0 when `main` returns (or returns `Ok(())`), status 1 when it
returns `Err(e)` (written as the one line `error: e`) or fails, and any other
status only through `process.exit`. A value at the end of `main` would have
nowhere to go: it is not printed, not an exit code, and an `Option` that is
`None` would end the program "well" without a word. So it is an error at
check, with the way out in its help: print it, give it to a host function,
or turn a missing value into an `Err` with `option.to_result(x, "why")?`. A
program that runs inside another (see the [embedding guide](../ffi.md))
hands its values to the host through a host function it calls;
`run_program` says only whether the program ended well.

## Float Safety

Silt has one float type, and a `Float` is always finite: an operation whose
result would be NaN or infinite is a **runtime error**, the same rule as
integer overflow. So floats compare, sort and hash as ordinary numbers, with
no NaN that is unequal to itself, and work as map keys and set elements. The
rule, its messages and how to guard an input are in
[Types: Finite floats](types.md#finite-floats).

## No Negative Indexing

`list.get(xs, -1)` is `None`, not "last element": a negative index is no
position of the list, like one past the end. Indices are positions from the
start, period. Use `list.last(xs)` for the last element,
or `list.get(xs, list.length(xs) - 1)` for explicit end-relative access.
This keeps the mental model simple and avoids hidden "if negative, wrap"
logic.

## Arguments Out of Range

A standard library function meets an argument outside its range in one of
three ways, by what the argument says:

- **A position to look up** that is not there gives `None`: `list.get`,
  `bytes.get`, `string.split_at`, `list.set` past the end.
- **A count, width or position that says how much** is cut to the range: a
  negative one counts as 0, as one past the end counts as the end. This is
  `list.take`, `list.drop`, `stream.take`, `stream.drop`, `string.slice`,
  `bytes.slice`, `string.repeat` and the width of `string.pad_left`,
  `string.pad_right`, `string.left` and `string.right`. A count worked
  out from data, as in `list.take(xs, n - k)`, so never ends a program.
- **An argument that no data can mean** is a mistake in the code, and a
  runtime error: a size of 0 or less for `list.chunks` and `stream.chunks`,
  `int.clamp` with the low bound above the high one, a pad that is not one
  character, negative decimals for `float.format`, month 13 for
  `time.days_in_month`.

```silt
import list
import string

fn main() {
  println(list.take([1, 2, 3], -1)) -- []
  println(string.slice("hello", -2, 2)) -- he
  println(list.get([1, 2, 3], 5)) -- None
}
```

## Immutability Cost

DP and graph algorithms must thread state through `loop` or `fold`. More
verbose, but enables concurrency safety and reasoning guarantees.

## Newline Sensitivity

A newline ends a statement, so silt needs no semicolons. A line continues
the one before it only where that cannot be misread: an infix operator such
as `|>`, `.`, `==` or `*` at the start of a line continues it, while `+` and
`-` must end the line they continue (`-` at the start of a line is a
negation), and a call, `?` or trailing closure must stay on the line of what
it applies to. The full rule, with examples, is in
[Operators: Newline Sensitivity](operators.md#newline-sensitivity).
