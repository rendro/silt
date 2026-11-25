---
title: "Repeating, Pipes, and Other Features"
section: "Language"
order: 6
---

# Repeating, Pipes, and Other Features

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


## Repeating

silt has no `loop`, `while` or `for`. A program repeats in two ways:
the library walks a list, a range or a channel, and a function calls
itself.

### The library first

Most repeating is a walk over something that is already there. A range
`1..n` is a list that holds no element until one is read, so counting
is a walk too:

```silt
import list

fn main() {
  list.each(1..3) { i -> println("round {i}") }
  let total = list.fold(1..100, 0) { acc, i -> acc + i }
  println(total) -- 5050
  let first_big = [3, 8, 12, 5]
    |> list.fold_until(None) { _, x ->
      match x > 10 {
        true -> Stop(Some(x))
        _ -> Continue(None)
      }
    }
  println(first_big) -- Some(12)
}
```

- `list.each`, `list.map`, `list.filter` and `list.fold` go over every
  element; `list.fold_until` stops at the first `Stop(value)` and goes on
  at each `Continue(state)`.
- `channel.each` takes every message until the channel is closed, and
  `channel.fold_until` keeps a state over the messages, with the same
  `Continue` and `Stop`: it is how an actor keeps its state, and an arm
  that forgets to say whether it goes on does not type-check
  ([channel.fold_until](../stdlib/channel-task.md#channelfold_until)).
  `stream.fold_until` does the same over a stream.

Each round of a fold is a call of its closure. A step budget and a
deadline count those rounds as they count any other code, so a long fold
in a task gives way to other tasks and stops at its deadline.

### A function that calls itself

When there is nothing to walk ("ask until the answer is right", "serve
until the channel closes"), a function calls itself. A top-level `fn` may
call itself and any other top-level function, and a closure bound by a
`let` may call itself by its name:

```silt
import channel

fn drain(ch: Channel(Int), seen: Int) -> Int {
  match channel.receive(ch) {
    Some(_) -> drain(ch, seen + 1)
    None -> seen
  }
}

fn main() {
  let ch = channel.new(4)
  channel.send(ch, 1)
  channel.send(ch, 2)
  channel.close(ch)
  println(drain(ch, 0)) -- 2
  let limit = 3
  let count = { i, acc ->
    match i > limit {
      true -> acc
      false -> count(i + 1, acc + i)
    }
  }
  println(count(1, 0)) -- 6
}
```

The closure `count` sees the names around it (`limit`), so what stays the
same from round to round is not passed along. What changes is passed as
arguments, and the call that starts the repeating gives the first values.
A `return` or a `?` inside a closure ends the closure, not the function
around it. Two closures cannot call each other; functions that call each
other are top-level functions.

**A call that comes last is a jump.** A call of a function to itself as
the last thing it does (a tail call), as `drain(ch, seen + 1)` is here,
jumps back to the start of the function: it takes no frame however many
rounds there are, every round costs the same, and the call stack of an
error shows the function once. A tail call of another function (`idle`
handing over to `running`, which hands back) takes no frame either, but
it is a call, which costs more than a jump, and the call stack shows the
latest of those hand-overs. In the REPL a top-level function's call of
itself is a call too: it reads the function's definition each round, as
every use of a top-level name there does.

A call comes last as the last statement of the body, as the value of a
match arm or of a block that comes last, and behind `return`. The else of
a `when` must end, so a `when` whose else goes on writes `return` before
the call:

```silt
fn sum_even(i: Int, n: Int, acc: Int) -> Int {
  when i < n else {
    return acc
  }
  when i % 2 == 0 else {
    return sum_even(i + 1, n, acc)
  }
  sum_even(i + 1, n, acc + i)
}

fn main() {
  println(sum_even(0, 10, 0)) -- 20
}
```

A function that calls itself for ever (a server, a worker) is stopped as
any other code is: a deadline ends it, and the scope of its task stops it
when the scope ends.

### What the compiler checks

A function that calls itself, or a group of functions that call each
other, is checked for what it costs in stack. The editor shows the
verdict on the function's name:

- **constant stack**: every call between them is a tail call. It runs
  for as many rounds as it likes.
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
import list

type Tree {
  Node(Int, List(Tree)),
}

fn size(t: Tree) -> Int {
  match t {
    Node(_, kids) -> 1 + list.fold(kids, 0) { total, kid -> total + size(kid) }
  }
}

fn main() {
  println(size(Node(1, [Node(2, []), Node(3, [Node(4, [])])]))) -- 4
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
import list

deep fn quicksort(xs: List(Int)) -> List(Int) {
  match xs {
    [] -> []
    [pivot, ..rest] -> list.concat(
      quicksort(list.filter(rest) { x -> x < pivot }),
      [pivot, ..quicksort(list.filter(rest) { x -> x >= pivot })],
    )
  }
}

fn main() {
  println(quicksort([3, 1, 2])) -- [1, 2, 3]
}
```

**A function that gives `()` says where it ends.** When every call of a
group is a tail call and a function of it gives `()`, each place it ends
without calling on must be `()`, `return e`, or a call of a function that
gives `Never`. A call that gives `()` there, or a block that ends in a
`let`, is an error (`E0346`): it is where a forgotten call to go on
hides.

```silt
import channel

fn serve(inbox: Channel(String), seen: Int) {
  match channel.receive(inbox) {
    -- It ends here, and says so.
    Some("stop") -> return println("stopped after {seen}")
    Some(_) -> serve(inbox, seen + 1)
    None -> ()
  }
}

fn main() {
  let inbox = channel.new(4)
  channel.send(inbox, "a")
  channel.send(inbox, "stop")
  serve(inbox, 0)
}
```

The rule is about functions that give `()`; one that gives a value (a
`Result`, an `Option`) is not checked for it. For an actor that gives
a value, `channel.fold_until` asks every arm to say `Continue` or `Stop`.

**What the check does not see.** It reads direct calls. A call through
a trait (`Show.show(left)`, `x.show()`) is a call of the impl of the
argument's type and is not checked (the editor says "not checked: trait
call" on such a method), and neither is a function reached through a
value kept in data (a list, a tuple, a record, a map of handlers). A
`let` whose value is just a function's name (`let g = f`, also inside a
function, also an alias of an alias) is that function wherever it is
named. It catches the accidents (a call that silently takes a frame for
every round, a forgotten call to go on); it does not prove that every
program ends.

**A tail call lets go.** A call that ends a function runs in the
function's place, so what the function holds is let go before the call
runs. A `let` whose name starts with `_` (a value kept only for as long
as it lives) that is in scope at such a call gets a warning (`W0302`).

### The frame budget

A call whose result is still used, as in `x + sum(rest)`, takes a frame
until it returns. The frames of a program, its tasks' included, may take
128 MiB in all: about 700,000 levels of `x + sum(rest)` over a list (a
frame and the values it holds, about 190 bytes), more of a function with
fewer values. The data the values point to is not counted, and the first
64 KiB of each task's frames are its own. A recursion that would take more
ends with a runtime error and its call stack:

```
error[runtime]: stack overflow: call frames take more than 128 MiB; this task holds 128 MiB
```

The budget is the program's, so where tasks recurse at the same time, the
one whose frames ask for more once it is used up gets the error, and the
error says how much of it that task holds itself. Each task gives back
what its frames took as they shrink and when it ends.

The `SILT_FRAME_BUDGET` environment variable sets another budget, in MiB,
read once when the program starts: `SILT_FRAME_BUDGET=16 silt run main.silt`
ends a runaway recursion sooner. A value silt cannot read (`abc`, `0`,
`64MiB`) is refused: `silt run`, `silt test` and `silt repl` do not
start, and the error says why.


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
