---
title: "Loops, Pipes, and Other Features"
---

# Loops, Pipes, and Other Features

## Pipe Operator

`|>` passes the left value as the **first argument** to the right side:

```silt
-- These are equivalent:
list.filter(xs, fn(x) { x > 0 })
xs |> list.filter { x -> x > 0 }
```

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
  |> list.each { u ->
    println("{u.name} is now {u.age}")
  }
}
```

**Why first-argument insertion?** Matches Elixir's convention. Works well
with collection functions where the collection is the natural first parameter.

**Why no auto-currying?** (1) Complicates error messages -- is `f(a)` a bug
or partial application? (2) Creates ambiguity with zero-argument calls. (3)
First-arg insertion is simpler to implement and explain.

**Trade-off:** you cannot partially apply functions through `|>`. Use
anonymous functions: `xs |> list.fold(0) { acc, x -> acc + x }`.


## String Interpolation

Silt strings support inline expressions with curly braces:

```silt
let name = "world"
let greeting = "hello {name}"          -- "hello world"
let math = "sum is {1 + 2 + 3}"       -- "sum is 6"
println("{user.name} is {user.age}")   -- field access in interpolation
```

String interpolation automatically invokes the `Display` trait. All types
(primitive and user-defined) implement `Display` automatically. No need to
call `.display()` explicitly.

Escape literal braces with backslash: `"\{not interpolation}"`.

### Triple-Quoted Strings

No escape processing, no interpolation, indentation stripping:

```silt
let json = """
  {
    "name": "Alice",
    "age": 30
  }
  """
```

The closing `"""` indentation determines whitespace stripping. Useful for
regex patterns with `{N}` quantifiers that would conflict with interpolation:

```silt
let regex = """[\w]+@[\w]+\.\w{2,}"""
```

**Design rationale.** No string concatenation operator exists. Interpolation
`"{a}{b}"` is the only inline way to build strings. For pipeline contexts,
use `string.join`. This keeps the string model simple and eliminates the
`"hello " + name + "!"` anti-pattern.


## Loop Expression

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

When the body produces a value without calling `loop(...)`, that value is the
result of the entire expression. `loop` is composable -- you can bind its
result, return it, or use it in a pipeline.

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
  match acc + x > 6 { true -> Stop(acc), _ -> Continue(acc + x) }
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
is cheap. All `list.*` functions work on ranges directly.

```silt
1..10
|> list.map { n -> n * n }
|> list.each { n -> println("{n}") }
```

## Comments

Line comments with `--`. Block comments with `{-` and `-}` (nestable):

```silt
-- line comment
let x = 42  -- inline comment

{-
  Block comment.
  {- Nested block comment -}
-}
```

## Operators

| Category   | Operators                                |
|------------|------------------------------------------|
| Arithmetic | `+`, `-`, `*`, `/`, `%`                  |
| Comparison | `==`, `!=`, `<`, `>`, `<=`, `>=`         |
| Boolean    | `&&`, `||`, `!`                           |
| Pipe       | `|>`                                      |
| Field      | `.`                                       |
| Range      | `..`                                      |
| Question   | `?` (error propagation)                   |
| Float recovery | `else` (narrows ExtFloat → Float)        |
