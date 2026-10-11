---
title: "Loops, Pipes, and Other Features"
section: "Language"
order: 6
---

# Loops, Pipes, and Other Features

## Pipe Operator

`|>` passes the left value as the **first argument** to the right side:

```silt
-- These are equivalent:
let a = list.filter(xs, { x -> x > 0 })
let b = xs |> list.filter { x -> x > 0 }
```

The right side is a call like any other, so the rule is the same when it
is a method call: the piped value is the first argument the call writes,
after the receiver. `5 |> acc.add()` is `acc.add(5)`.

Without pipes, function composition nests inside-out. With pipes, it reads
top-to-bottom:

```silt
[1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
-- result: 120
```

### Real-World Pipeline

```silt
import list

type User {
  name: String,
  age: Int,
  active: Bool,
}

fn birthday(user: User) -> User {
  user.{ age: user.age + 1 }
}

fn main() {
  let users = [
    User { name: "Alice", age: 30, active: true },
    User { name: "Bob", age: 25, active: false },
  ]

  users
    |> list.filter { u -> u.active }
    |> list.map { u -> birthday(u) }
    |> list.each { u -> println("{u.name} is now {u.age}") }
}
```

**Why first-argument insertion?** Matches Elixir's convention. Works well
with collection functions where the collection is the natural first parameter.

**Why no auto-currying?** (1) Complicates error messages -- is `f(a)` a bug
or partial application? (2) Creates ambiguity with zero-argument calls. (3)
First-arg insertion is simpler to implement and explain.

**Trade-off:** you cannot partially apply functions through `|>`. Use
anonymous functions: `xs |> list.fold(0) { acc, x -> acc + x }`.

### Pipe and `?`

A trailing `?` on a pipeline applies to the whole piped expression — no
parentheses needed. `x |> f |> g?` parses as `(x |> f |> g)?`. (Everywhere
else `?` binds tightly, like a call.) This means stdlib chains like
`io.read_file(path) |> result.map_err(Wrap)?` compose without parens around
the pipeline.


## String Interpolation

Silt strings support inline expressions with curly braces:

```silt
let name = "world"
let greeting = "hello {name}" -- "hello world"
let sum = "sum is {1 + 2 + 3}" -- "sum is 6"
println("{user.name} is {user.age}") -- field access in interpolation
```

String interpolation automatically invokes the `Display` trait. User-defined
types and the displayable builtins implement `Display` automatically;
channels and function values are the exception — they do not implement
`Display`, so interpolating them is a compile error. No need to call
`.display()` explicitly.

Escape literal braces with backslash: `"\{not interpolation}"`.

The expression between the braces may span lines, and a line break may
stand in front of the closing brace.

### Triple-Quoted Strings

No escape processing, no interpolation, indentation stripping:

```silt
let json_text = """
  {
    "name": "Alice",
    "age": 30
  }
  """
```

The closing `"""` indentation determines whitespace stripping. Useful for
regex patterns with `{N}` quantifiers that would conflict with interpolation:

```silt
let pattern = """[\w]+@[\w]+\.\w{2,}"""
```

**Design rationale.** Interpolation `"{a}{b}"` is the way to build
strings (`+` is arithmetic only) — it reads naturally and keeps
multi-fragment messages punctuation-light. For pipeline contexts, use
`string.join`.


## Infinite Loops

`loop { body }` runs the body **once** -- it is not an implicit infinite loop.
The `loop` form only re-enters its body when the body explicitly calls
`loop(...)` (the labeled re-entry call). To loop indefinitely, bind a sentinel
state variable and recur with `loop(...)` from inside the body:

```silt
loop _ = () {
  match channel.receive(ch) {
    channel.Message(val) -> {
      process(val)
      loop(()) -- re-enter to keep going
    }
    channel.Closed -> () -- fall through; loop returns
    _ -> loop(())
  }
}
```

`return` from an enclosing `fn` works as well. If you find yourself reaching
for "infinite loop" semantics, a recursive `fn` is often clearer:

```silt
fn drain(ch) {
  match channel.receive(ch) {
    channel.Message(val) -> {
      process(val)
      drain(ch)
    }
    channel.Closed -> ()
    _ -> drain(ch)
  }
}
```

This form is useful in concurrency patterns where a task should keep running
until a channel closes or some condition is met.


## Loop with State

`loop` is an expression that binds state variables and re-enters via
`loop(new_values)`:

```silt
fn sum(xs) {
  loop remaining = xs, total = 0 {
    match remaining {
      [] -> total
      [head, ..tail] -> loop(tail, total + head)
    }
  }
}
```

The initial values are read where the `loop` stands, all of them, before
any of the loop's names exists: in `loop n = n + 1, acc = n` both `n` on
the right are the one outside, as the arguments of `loop(...)` are the
values of the round before. The loop's names are known in its body only.

When the body produces a value without calling `loop(...)`, that value is the
result of the entire expression. `loop` is composable -- you can bind its
result, return it, or use it in a pipeline.

**`loop(...)` must be in tail position.** It jumps back to the top of the
loop with new values and never produces a value where it is written (its
type is `Never`), so nothing may use its result. It may appear only as the
last expression of the loop body, or of a block, a match arm or a `when`
else body that is itself in tail position:

```silt
fn fact(n) {
  loop i = n, acc = 1 {
    match i {
      0 -> acc
      _ -> loop(i - 1, acc * i) -- OK: last expression of a tail arm
    }
  }
}
```

`i * loop(i - 1)` is a type error: carry the running value in a binding
instead, as `acc` does above.

**Loop inside closures.** `loop()` works inside closures, which is useful for
search patterns:

```silt
fn find_index(xs, predicate) {
  loop remaining = xs, idx = 0 {
    match remaining {
      [] -> None
      [head, ..tail] -> match predicate(head) {
        true -> Some(idx)
        _ -> loop(tail, idx + 1)
      }
    }
  }
}
```

### Loop vs. `fold_until`

`list.fold_until` requires `Stop(value)` and `Continue(value)` to carry the
**same type**. Use `loop` when the result type differs from the iteration
state:

```silt
-- fold_until: accumulator IS the result
[1, 2, 3] |> list.fold_until(0) { acc, x ->
  match acc + x > 6 {
    true -> list.Stop(acc)
    _ -> list.Continue(acc + x)
  }
}

-- loop: state is (queue, visited) but result is Option(node)
fn bfs(graph, start, goal) {
  loop queue = [start], visited = [start] {
    match queue {
      [] -> None
      [node, ..rest] -> match node == goal {
        true -> Some(node)
        _ -> {
          let neighbors = map.get(graph, node) |> option.unwrap_or([])
          let new = neighbors |> list.filter { n -> !list.contains(visited, n) }
          loop(list.concat(rest, new), list.concat(visited, new))
        }
      }
    }
  }
}
```


## Ranges

The `..` operator creates an inclusive range. `1..10` includes both 1 and 10.
Ranges are lazy — they don't allocate memory until iterated, so `1..1000000`
is cheap. A range is a list: all `list.*` functions work on it, and
`println(1..3)` prints `[1, 2, 3]`.

```silt
1..10 |> list.map { n -> n * n } |> list.each { n -> println("{n}") }
```

## Comments

Line comments with `--`. Block comments with `{-` and `-}` (nestable):

```silt
-- line comment
let x = 42 -- inline comment

{-
  Block comment.
  {- Nested block comment -}
-}
```

## Operators

silt has arithmetic (`+`, `-`, `*`, `/`, `%`), comparison (`==`, `!=`,
`<`, `>`, `<=`, `>=`), boolean (`&&`, `||`, `!`), pipe (`|>`), range
(`..`), error propagation (`?`), field / record-update (`.`), type
ascription (`as`), and float recovery (`else`).

See [Operators and Precedence](operators.md) for the full table with
precedence, associativity, and newline-sensitivity rules.
