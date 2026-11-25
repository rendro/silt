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

The right side is a call like any other, so the rule is the same for a
call through a trait: `5 |> Greet.greet` is `Greet.greet(5)`, and
`5 |> Adder.add(3)` is `Adder.add(5, 3)`. A stage that takes the piped
value alone is written as the name: `xs |> list.sum`, which `silt fmt`
writes for `xs |> list.sum()`.

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
`Display`, so interpolating them is a compile error.

Escape literal braces with backslash: `"\{not interpolation}"`.

The expression between the braces may span lines, and a line break may
stand in front of the closing brace.

### Escapes

A backslash in a string starts an escape. These are all of them:

| Escape      | Character                                             |
|-------------|-------------------------------------------------------|
| `\n`        | line feed (U+000A)                                    |
| `\r`        | carriage return (U+000D)                              |
| `\t`        | tab (U+0009)                                          |
| `\0`        | NUL (U+0000)                                          |
| `\\`        | backslash                                             |
| `\"`        | double quote                                          |
| `\{`        | `{`, which otherwise starts an interpolation          |
| `\}`        | `}`                                                   |
| `\u{1F600}` | the Unicode character with that code: 1 to 6 hex digits, no surrogate (`D800` to `DFFF`), at most `10FFFF` |

```silt
import string

fn main() {
  println("caf\u{e9} \u{1F600}") -- café 😀
  println(string.length("a\r\n")) -- 3
  println(string.char_code("\0")) -- Some(0)
}
```

Any other character behind a backslash is an error. Triple-quoted
strings take no escapes: a backslash in one is a backslash.

A text-direction control character (U+202A to U+202E, U+2066 to
U+2069) is an error anywhere in a file, in a string or a comment too:
it can make the text show otherwise than the compiler reads it. In a
string, write it as an escape, `"\u{202E}"`.

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

A run of three or more quotes ends the string: its last three close it,
and the ones before are text, so `"""v = "s""""` is `v = "s"`. (A text
cannot hold three quotes in a row, except at its end: `"""a""""""` is
`a"""`.)

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
    Some(val) -> {
      process(val)
      loop(()) -- re-enter to keep going
    }
    None -> () -- fall through; loop returns
  }
}
```

`return` from an enclosing `fn` works as well. If you find yourself reaching
for "infinite loop" semantics, a recursive `fn` is often clearer:

```silt
fn drain(ch) {
  match channel.receive(ch) {
    Some(val) -> {
      process(val)
      drain(ch)
    }
    None -> ()
  }
}
```

This form is useful in concurrency patterns where a task should keep running
until a channel closes or some condition is met.

A call of a function to itself in tail position, as `drain(ch)` is here, is
a jump back to the start of the function, as `loop(...)` is: a round costs
what a round of a loop costs, it takes no frame however many rounds there
are, and the call stack of an error shows the function once. A tail call of
another function (`idle` handing over to `running`, which hands back) takes
no frame either, but it is a call, which costs more than a jump, and the
call stack shows the latest of those hand-overs. In the REPL a top-level
function's call of itself is a call too: it reads the function's
definition each round, as every use of a top-level name there does.


## Repeating

A function that calls itself, or a group of functions that call each
other, is checked for what it costs in stack. The editor shows the
verdict on the function's name:

- **constant stack**: every call between them is a tail call (the last
  thing the function does). It runs for as many rounds as it likes.
- **depth follows the data**: each one descends on a parameter. Every
  call that is not a tail call passes a *part* of the caller's parameter
  in the callee's place, and every tail call passes the parameter itself
  or a part of it. It goes as deep as its data is, no deeper.
- anything else is an error, `E0345`.

A part is a name a pattern binds strictly inside the parameter: `rest`
in `[x, ..rest]`, `l` and `r` in `Node(l, v, r)`, a field read
(`t.kids`), and so on down. The parameter's own name in an arm
(`all -> ...`) is the whole value, not a part, and so is a name bound
again by a `let`. A closure handed to `list.each`, `list.map`,
`list.flat_map`, `list.fold`, `list.filter`, `list.find`, `list.any` or
`list.all` over a part may call the function on the element, and
`list.map(kids, walk)` passes it each element:

```silt
type Tree {
  Node(Int, List(Tree)),
}

fn size(t: Tree) -> Int {
  match t {
    Node(_, kids) -> 1 + list.fold(kids, 0) { total, kid -> total + size(kid) }
  }
}
```

A count down is not a part. `n + sum_to(n - 1)` is refused, with the
fold or a parameter that carries the running value as the way out:

```
error[type]: this call of `sum_to` takes a frame for every round
 --> sum.silt:4:14
  |
4 |     _ -> n + sum_to(n - 1)
  |              ^^^^^^^^^^^^^ this call of `sum_to` takes a frame for every round
  = note: not the last thing `sum_to` does, and `n - 1` is not a part of `n`
  = help: count with the library: `list.fold(1..n, start) { acc, i -> ... }`
  = help: or carry the running value in a parameter, so that the call comes last
  = help: if its depth is bounded by something the compiler cannot see, write `deep fn sum_to`
```

**`deep fn`.** When the depth is bounded by something the compiler
cannot see (quicksort calls itself on filtered lists, a parser on the
rest of its input), write `deep` before `fn`: the check is off for that
function's calls, and the editor says "deep (not checked)". It is normal
in parsers and interpreters. `deep` is a word of its own only there; a
`let` closure that needs it becomes a top-level function. A `deep` the
check would not need is a warning (`W0301`).

```silt
deep fn quicksort(xs: List(Int)) -> List(Int) {
  match xs {
    [] -> []
    [pivot, ..rest] -> list.concat(
      quicksort(list.filter(rest) { x -> x < pivot }),
      [pivot, ..quicksort(list.filter(rest) { x -> x >= pivot })],
    )
  }
}
```

**A `()` loop says where it ends.** When every call of a group is a tail
call and a function of it gives `()`, each place it ends without calling
on must be `()`, `return e`, or a call of a function that gives
`Never`. A call that gives `()` there, or a block that ends in a `let`,
is an error (`E0346`): it is where a forgotten call to go on hides.

```silt
fn serve(inbox: Channel(String)) {
  match channel.receive(inbox) {
    Some("flush") -> {
      flush()
      serve(inbox)
    }
    Some(line) -> return println(line) -- stops here, and says so
    None -> ()
  }
}
```

The rule is about loops that give `()`; one that gives a value (a
`Result`, an `Option`) is not checked for it.

**What the check does not see.** It reads direct calls. A call through
a trait (`Show.show(left)`, `x.show()`) is a call of the impl of the
argument's type and is not checked (the editor says "not checked: trait
call" on such a method), and neither is a function reached through a
value. It catches the accidents (a call that silently takes a frame for
every round, a forgotten call to go on); it does not prove that every
program ends.

**A tail call lets go.** A call that ends a function runs in the
function's place, so what the function holds is let go before the call
runs. A `let` whose name starts with `_` (a value kept only for as long
as it lives) that is in scope at such a call gets a warning (`W0302`).


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

**Loops and closures.** A closure's body can hold a `loop` of its own. A
`loop(...)` written inside a closure cannot go on with a loop outside the
closure: the closure runs when it is called, not as the loop's body, so
that is an error. Call the closure in the loop's body and decide there, as
this search does with its predicate:

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
    true -> Stop(acc)
    _ -> Continue(acc + x)
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
(`..`), error propagation (`?`), field / record-update (`.`), and type
ascription (`as`).

See [Operators and Precedence](operators.md) for the full table with
precedence, associativity, and newline-sensitivity rules.
