---
title: "Bindings and Functions"
section: "Language"
order: 1
---

# Bindings and Functions

## Philosophy

### Keywords

Silt has a small, fixed keyword set. The full keyword list is:

```
as  else  fn  import  let  loop  match  mod
pub  return  trait  type  when  where
```

This is a forcing function, not an aesthetic choice. Every time we considered
adding a keyword (`if`, `for`, `while`, `mut`, `async`, `await`,
`catch`, `throw`...), we asked: "Can an existing construct handle this?" The
answer was almost always yes.

- `if`/`else` is subsumed by `match`.
- General-purpose iteration uses `loop`, collection traversal uses
  higher-order functions (`list.map`, `list.filter`, `list.fold`).
- `mut` does not exist because nothing is mutable.
- `async`/`await` does not exist because concurrency is CSP-based.
- `try`/`catch` does not exist because errors are values.

Concurrency primitives live in modules (`channel.new`, `channel.send`,
`task.spawn`, etc.) rather than as keywords — this keeps the global
namespace clean and avoids the PHP problem of too many bare globals.

The following names are always available without an import: `print`,
`println`, `panic`, `Ok`, `Err`, `Some`, `None`, plus the primitive type
descriptors `Int`, `Float`, `String`, and `Bool` (used with
type-directed APIs like `json.parse_map`). Everything else is reached
through its module, the stdlib's types and constructors included:
`list.Stop(acc)`, `channel.Message(v)`, `time.Monday`, `http.GET` (after
`import list`, `import channel`, ...), or listed in a selective import
(`import channel.{ Message }`).

What is _not_ a keyword matters too. `true`/`false` are builtin literals.
`Ok`, `Err`, `Some`, `None` are builtin variant constructors -- ordinary
values defined in the prelude. `_` is a wildcard pattern token. This keeps
the keyword count honest.

### Expression-Based

Every construct in Silt is an expression. A `match` returns a value. A block
returns its last expression. There are no "statements" -- `let` and `when`
are statement-level forms inside blocks, but blocks themselves are expressions.

```silt
let description = match shape {
  Circle(r) -> "circle with radius {r}"
  Rect(w, h) -> "rect {w}x{h}"
}

let result = {
  let x = compute()
  let y = transform(x)
  x + y
}
```

The trade-off: functions that exist only for side effects return `()` (Unit).

Only the last expression of a block is the block's value. An expression that
stands before it has no one to give its value to, so it must have none: its
type is `()`, or it never returns (`return`, `panic`). Anything else is an
error, and `let _ = ...` is how a value is discarded on purpose:

```silt
import io
import task

fn main() {
  let _ = task.spawn { -> println("in the background") }
  match io.write_file("out.txt", "data") {
    Ok(_) -> println("saved")
    Err(e) -> println("not saved: {e.message()}")
  }
}
```

Without the `let _ =`, the first line is rejected: ``this `Handle(())` value
is unused; write `let _ = ...` to discard it``. A `Result` left unused is how
an error goes unseen, so its message asks for more: handle it, return it with
`?`, or discard it with `let _ = ...`. A call whose type nothing else decides
(`f()` where `f` is a parameter) has type `()` when it stands as a statement.

### Immutability as Default (and Only Option)

All bindings are immutable. There is no `mut`, no mutable references, no
assignment to existing bindings. Inside a function body, shadowing is
allowed:

```silt
fn main() {
  let x = 42
  let x = x + 1 -- shadowing, not mutation
  println(x)
}
```

Why no mutation at all? (1) Concurrency safety — immutable values need no
locks. (2) Simpler reasoning — values never change after creation.

The trade-off is real: algorithms that naturally use mutation (in-place
sorting, graph traversal with visited sets) require recursion or functional
combinators. Record update syntax (`user.{ age: 31 }`) is the mitigation —
it looks like mutation but always returns a new value.

### Explicit Over Implicit

Silt has no exceptions, no null, no implicit conversions, no implicit error
propagation. `1 + 1.0` is a type error. If a function can fail, its return
type says so. If a value might be absent, its type says so. If control flow
can exit early, the syntax (`?` or `when`-`else`) says so.

### One Way to Do Things

`match` subsumes `if`. `loop` subsumes `while`. String interpolation
`"{a}{b}"` subsumes concatenation. Module-qualified functions subsume bare
globals. When there is one way, every Silt program reads the same way.


## Language Features

### Bindings

Every value is bound with `let`. No `var`, no `mut`, no reassignment:

```silt
let x = 42
let name = "Robert"
```

**Shadowing** creates a new binding with the same name inside a function
body:

```silt
fn main() {
  let x = 1
  let x = x + 1 -- x is now 2; the original 1 is untouched
  println(x)
}
```

At the top level a name is bound only once. Two top-level declarations
that bind the same name — two imports of it (`import a.{ x }` and
`import b.{ x }`), two `import m as n` with the same alias, or an import
and a top-level `fn`, `type`, `let` or trait — are an error that names
both sites. Which declaration a top-level name refers to therefore never
depends on the order of the declarations.

Nor does the value of a top-level `let`. A top-level `let` is
initialised after every top-level `let` its value can reach: the ones
it names, and the ones that the functions and methods it mentions read.
Where nothing orders two of them, the one written first runs first.

```silt
let total = base() + 1 -- runs second: `base` reads `start`

let start = 10

fn base() -> Int {
  start * 2
}

fn main() {
  println(total) -- 21
}
```

What a `let`'s value can reach is decided by four rules:

- **A function that is named counts as called.** So does a function of
  another module, and code of another module can call the methods this
  module's impls give that module's traits (and the builtin traits):
  a `let` that mentions another module's function is initialised after
  every `let` those methods read.
- **A method call on a value whose type is a bounded type variable
  (`fn f(x: a) where a: Label { x.label() }`) reaches every impl of the
  method in the module**, whichever value the call is given.
- **A `let` that is a plain value runs nothing.** A plain value is a
  literal, a name, a closure, or a list, tuple, record, map, set or
  variant made of plain values. Such a `let` needs only the top-level
  `let`s it names outside closures; what the functions it names read
  does not matter. A table of handlers that read the table is fine:

```silt
import list

let routes = [("home", home), ("count", count)]

fn home() -> String {
  "home"
}

fn count() -> String {
  "{list.length(routes)} routes"
}

fn main() {
  println(count()) -- 2 routes
}
```

A closure handed to a function is not a plain value: `let f = wrap({ n
-> ... f(n - 1) ... })` runs `wrap`, which may call the closure.

- **Showing a value reaches the `Display` impls of its parts.**
  Interpolation, `println`, `string.from` and `.display()` call the
  `Display` impl written for each type the value is made of: its own,
  its fields', its type arguments'. A value of an unknown type (a type
  variable, an associated type such as `Self::Item`) may be of any
  type: showing it reaches every `Display` impl of the module.

A function that shows a value of its own type variable shows what it is
given, so that showing counts where the function is named, at the type
it is named at. `label(42)` shows an `Int`, which calls no impl:

```silt
import string

type Row {
  n: Int,
}

fn label(x: a) -> String where a: Display {
  "<{x}>"
}

let width = string.length(label(42)) -- shows an Int: reaches no impl

trait Display for Row {
  fn display(self) -> String {
    string.pad_left("{self.n}", width, "0")
  }
}

fn main() {
  println(Row { n: 1 }) -- 0001
}
```

This goes one step only. A function that hands its own type variable on
(`fn wrap(x: a) -> String where a: Display { label(x) }`) names `label`
at a type that is not known there, so `wrap(42)` reaches every `Display`
impl of the module, and `let width = string.length(wrap(42))` above
would be an error: `width -> wrap -> Row.display -> width`. The way out
is to call `label` at the known type (`label(42)`), or to compute the
value without showing (`let width = 4`).

A top-level `let` that can reach itself has no place in the order, and
is an error that names the way round:

```silt
let a = f()

fn f() -> Int {
  b + 1
}

let b = a + 1
-- error: the top-level `let` 'a' needs its own value to be initialised: a -> f -> b -> a
```

**Destructuring** works in `let` for irrefutable patterns, the ones that
match every value of their type: tuples and records, the constructor of a
single-variant type, an or-pattern whose alternatives cover the type
between them, and any nesting of these:

```silt
let (x, y) = (1, "hello")
let User { name, age, .. } = user
```

List patterns like `[a, b, c]` are *refutable* (they fail when the list has a
different length), so `let [a, b, c] = xs` is rejected. Use `match` or
`when let ... else` for those:

```silt
match xs {
  [a, b, c] -> use(a, b, c)
  _ -> handle_other_shape()
}

-- `when let` is a statement that binds in the surrounding scope on success;
-- the `else` branch must diverge (return or panic) so the bindings are sound.
when let [a, b, c] = xs else {
  return handle_other_shape()
}
use(a, b, c)
```

**Type annotations** are optional (Hindley-Milner infers everything) but
useful for documentation:

```silt
let x: Int = 42
let transform: Fn(Int) -> Int = { x -> x * 2 }
```


### Functions

**Named functions** use block bodies. The last expression is the return value:

```silt
fn add(a, b) {
  a + b
}

fn square(x) {
  x * x
}
```

**Parameters** of named functions, trait methods and closures take the same
forms: a name or an irrefutable destructuring pattern (a tuple, a record, or
the constructor of a single-variant type), each with an optional type
annotation. A refutable pattern such as `Some(x)` or `[a, b]` is rejected;
bind a name and `match` on it in the body instead.

```silt
fn add(x: Int, y: Int) {
  x + y
}

fn first((a, b): (Int, String)) {
  a
}

fn area(Point { width, height }) {
  width * height
}
```

**Closures** are values that close over their environment. A closure is
written in braces: its parameters, `->`, and its body:

```silt
let double = { x -> x * 2 }

fn make_adder(n) {
  { x -> x + n }
}

let answer = { -> 42 } -- no parameters
```

A closure has no return-type annotation; its type is the type of its body:

```silt
let swap = { (a, b): (Int, String) -> (b, a) }
```

**No nested named functions.** Use `let f = { x -> ... }` for local helpers.
Named functions are always top-level, keeping scoping rules simple.

**Trailing closures:** when the last argument is a closure, write it outside
the parentheses:

```silt
[1, 2, 3] |> list.map { x -> x * 2 }
[1, 2, 3] |> list.fold(0) { acc, x -> acc + x }

-- Destructuring in closure parameters
pairs |> list.each { (n, word) -> println("{n} is {word}") }
```

**Return type annotations:**

```silt
fn add(a: Int, b: Int) -> Int {
  a + b
}
```

**Early return** with `return`. Both `return` and `panic()` produce the
`Never` type, which unifies with any other type -- so they can appear in any
expression position without causing type errors:

```silt
fn get_or_die(opt) {
  match opt {
    Some(v) -> v
    None -> panic("expected a value") -- Never unifies with v's type
  }
}
```
