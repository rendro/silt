---
title: "Language Guide"
order: 2
description: "A complete reference to silt's features: pattern matching, types, traits, closures, concurrency, and error handling."
---

# Silt Language Guide

Silt is a statically-typed, expression-based programming language with full
immutability, pattern matching as the sole branching construct, and CSP-style
concurrency. File extension: `.silt`.

This document is the complete language reference and design deep-dive. It
covers every feature, explains why it exists, and notes the trade-offs.


# Part 1: Philosophy

## 14 Keywords

Silt has exactly 14 keywords:

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

The global namespace has only 12 names: `print`, `println`, `panic`,
`Ok`, `Err`, `Some`, `None`, `Stop`, `Continue`, `Message`, `Closed`,
`Empty`. Everything else requires module qualification.

What is _not_ a keyword matters too. `true`/`false` are builtin literals.
`Ok`, `Err`, `Some`, `None` are builtin variant constructors -- ordinary
values defined in the prelude. `_` is a wildcard pattern token. This keeps
the keyword count honest.

## Expression-Based

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

## Immutability as Default (and Only Option)

All bindings are immutable. There is no `mut`, no mutable references, no
assignment to existing bindings. Shadowing is allowed:

```silt
let x = 42
let x = x + 1    -- shadowing, not mutation
```

Why no mutation at all? (1) Concurrency safety — immutable values need no
locks. (2) Simpler reasoning — values never change after creation.

The trade-off is real: algorithms that naturally use mutation (in-place
sorting, graph traversal with visited sets) require recursion or functional
combinators. Record update syntax (`user.{ age: 31 }`) is the mitigation —
it looks like mutation but always returns a new value.

## Explicit Over Implicit

Silt has no exceptions, no null, no implicit conversions, no implicit error
propagation. `1 + 1.0` is a type error. If a function can fail, its return
type says so. If a value might be absent, its type says so. If control flow
can exit early, the syntax (`?` or `when`-`else`) says so.

## One Way to Do Things

`match` subsumes `if`. `loop` subsumes `while`. String interpolation
`"{a}{b}"` subsumes concatenation. Module-qualified functions subsume bare
globals. When there is one way, every Silt program reads the same way.


# Part 2: Language Features

## 1. Bindings

Every value is bound with `let`. No `var`, no `mut`, no reassignment:

```silt
let x = 42
let name = "Robert"
```

**Shadowing** creates a new binding with the same name:

```silt
let x = 1
let x = x + 1   -- x is now 2; the original 1 is untouched
```

**Destructuring** works in `let` using the same pattern language as `match`:

```silt
let (x, y) = (1, "hello")
let [a, b, c] = [1, 2, 3]
let User { name, age, .. } = user
```

**Type annotations** are optional (Hindley-Milner infers everything) but
useful for documentation:

```silt
let x: Int = 42
let transform: Fn(Int) -> Int = fn(x) { x * 2 }
```


## 2. Functions

**Named functions** use block bodies. The last expression is the return value:

```silt
fn add(a, b) {
  a + b
}
```

**Single-expression shorthand** uses `=`:

```silt
fn square(x) = x * x
fn greet(name) = "hello {name}"
```

**Anonymous functions (closures)** are values that close over their environment:

```silt
let double = fn(x) { x * 2 }

fn make_adder(n) {
  fn(x) { x + n }
}
```

**No nested named functions.** Use `let f = fn(x) { ... }` for local helpers.
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
    None -> panic("expected a value")   -- Never unifies with v's type
  }
}
```


## 3. Types

### Primitive Types

| Type     | Description                       | Examples                  |
|----------|-----------------------------------|---------------------------|
| `Int`    | 64-bit signed integer (overflow is a runtime error) | `42`, `-7`, `0`           |
| `Float`  | 64-bit floating-point, guaranteed finite | `3.14`, `-0.5`, `1.0`    |
| `ExtFloat` | 64-bit floating-point (IEEE 754, allows NaN/Infinity) | Division and some math results |
| `Bool`   | Boolean                           | `true`, `false`           |
| `String` | UTF-8 string with interpolation   | `"hello"`, `"age: {n}"`  |
| `Unit`   | No meaningful value               | (returned by `println`)   |

No implicit conversions. Use `int.to_float()` or `float.to_int()` explicitly.

### Enums (Tagged Unions)

```silt
type Shape {
  Circle(Float)
  Rect(Float, Float)
}

type Color { Red, Green, Blue }
```

Constructors create values: `Circle(5.0)`, `Rect(3.0, 4.0)`. The compiler
checks exhaustiveness when you match on them.

### Generic Types

```silt
type Option(a) { Some(a), None }
type Result(a, e) { Ok(a), Err(e) }
```

Type parameters are filled in at use: `Option(Int)`, `Result(String, String)`.

### Records

```silt
type User {
  name: String,
  age: Int,
  active: Bool,
}

let alice = User { name: "Alice", age: 30, active: true }
alice.name   -- "Alice"
```

**Record update syntax** creates a new record with fields changed:

```silt
let alice2 = alice.{ age: 31 }
```

Read as "alice, but with age 31." Compare to Elm `{ u | age = 31 }`, Rust
`User { age: 31, ..u }`. Silt's `.{ }` syntax avoids new keywords or sigils.

### Tuples

Fixed-size, heterogeneous:

```silt
let pair = (1, "hello")
let (x, y) = pair
```

### Recursive Types

Types can reference themselves:

```silt
type Expr {
  Num(Int)
  Add(Expr, Expr)
}
```

### Function Type Annotations

```silt
let apply: Fn(Int, Int) -> Int = fn(a, b) { a + b }

type Handler {
  name: String,
  run: Fn(String) -> String,
}
```

### Type Ascription

When type inference cannot determine a type from context, use `as` to assert it:

```silt
let x = empty() as List(Int)
let r = (parse("42") as Result(Int, String))?
```

`as` is a compile-time assertion — if the types conflict, you get a type error.
At runtime it's a no-op.


## 4. Pattern Matching

Pattern matching is the only branching construct. No `if`, no ternary, no
`switch`. Every branch uses `match`, and the compiler checks exhaustiveness.

### Match with Scrutinee

```silt
fn describe(shape) {
  match shape {
    Circle(r) -> "circle with radius {r}"
    Rect(w, h) -> "rect {w}x{h}"
  }
}
```

### Match without Scrutinee (Boolean Dispatch)

Omit the scrutinee for boolean conditions:

```silt
fn classify(n) {
  match {
    n == 0 -> "zero"
    n > 0  -> "positive"
    _      -> "negative"
  }
}
```

### Literal Patterns

Including negative numbers:

```silt
match n {
  0 -> "zero"
  1 -> "one"
  -1 -> "negative one"
  _ -> "other"
}
```

Works for `Int`, `Float`, `String`, and `Bool`.

### Constructor and Nested Patterns

```silt
fn handle(result) {
  match result {
    Ok(Some(value)) -> use(value)
    Ok(None) -> handle_empty()
    Err(e) -> handle_error(e)
  }
}
```

### Tuple Patterns

```silt
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}
```

### Record Patterns

Use `..` to ignore remaining fields:

```silt
fn greet(user) {
  match user {
    User { name, active: true, .. } -> "hello {name}"
    User { name, .. } -> "{name} is inactive"
  }
}
```

### List Patterns

Three forms: empty `[]`, exact length `[a, b, c]`, head/tail `[h, ..t]`:

```silt
fn sum(xs) {
  match xs {
    [] -> 0
    [head, ..tail] -> head + sum(tail)
  }
}
```

Nested patterns work in list elements: `[Some(a), Some(b), ..rest]`. The
`..` syntax matches the record rest pattern (`User { name, .. }`).

### Map Patterns

```silt
match config {
  #{ "name": name, "greeting": greeting } -> "{greeting}, {name}!"
  #{ "name": name } -> "hello, {name}!"
  _ -> "hello, stranger!"
}
```

### Or-Patterns

Match multiple alternatives in a single arm with `|`:

```silt
match n {
  0 | 1 -> "small"
  2 | 3 -> "medium"
  _ -> "large"
}
```

All alternatives must bind the **same** variables:

```silt
-- OK: both sides bind x
Some(x) | Ok(x) -> use(x)

-- ERROR: left binds x, right binds y
Some(x) | Ok(y) -> ...   -- compile error
```

### Guards

```silt
match n {
  0 -> "zero"
  x when x > 0 -> "positive"
  _ -> "negative"
}
```

Guard expressions are checked after the pattern matches. Pattern bindings
are available in the guard.

### Range Patterns

Inclusive numeric ranges:

```silt
match score {
  90..100 -> "A"
  80..89  -> "B"
  70..79  -> "C"
  _       -> "F"
}
```

Float ranges work too: `0.0..1.0` matches `value >= 0.0 && value <= 1.0`.

### Pin Operator (`^`)

Matches against the value of an existing variable instead of creating a new
binding:

```silt
let expected = 42
match input {
  ^expected -> "got the expected value"
  other -> "got {other} instead"
}
```

Works in any pattern position. Common with `channel.select`:

```silt
match channel.select([ch1, ch2]) {
  (^ch1, Message(msg)) -> handle_first(msg)
  (^ch2, Message(msg)) -> handle_second(msg)
  _ -> panic("unexpected")
}
```

### `when`/`else` for Inline Assertions

Asserts a pattern match and binds on success, or diverges on failure:

```silt
fn process(input) {
  when Ok(value) = parse(input) else { return Err("parse failed") }
  when Some(user) = find_user(value) else { return Err("not found") }
  when Admin(perms) = user.role else { return Err("unauthorized") }
  do_admin_thing(user, perms)
}
```

The `else` block **must** diverge (`return` or `panic`). This flattens the
"staircase of doom" that nested `match` creates.

**Boolean form** -- also accepted, for flat guard sequences:

```silt
fn buy(qty, balance, price) {
  when qty > 0 else { return Err("out of stock") }
  when balance >= price else { return Err("not enough money") }
  Ok("purchased")
}
```

Both forms can be mixed freely in the same function.

### Exhaustiveness Checking

The compiler checks that your match covers all possible cases. Missing a
variant produces a compile-time error. This is one of the strongest benefits
of `match` as the sole branching construct.

**Trade-off: no `if`.** Simple boolean checks are more verbose (`match debug
{ true -> ..., false -> () }`). In practice, guardless match and `when`-`else`
cover most cases.


## 5. Pipe Operator

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


## 6. String Interpolation

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


## 7. Error Handling

### No Exceptions

Functions that can fail return `Result(value, error)`. Values that might be
absent are `Option(value)`. Errors are values -- visible in types, mandatory
to handle:

```silt
match parse_int(input) {
  Ok(n) -> use(n)
  Err(e) -> handle(e)
}
```

### The `?` Operator

Propagates errors to the caller. Works on both `Result` and `Option`:

```silt
fn process(input) {
  let n = parse_int(input)?       -- returns Err early if parse fails
  let result = validate(n)?
  Ok(result * 2)
}
```

### `when`-`else` for Custom Errors

When you need custom error messages or destructuring beyond `?`:

```silt
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  when Ok(port) = port_line |> string.replace("port=", "") |> int.parse() else {
    return Err("invalid port number")
  }

  Ok("connecting to {host}:{port}")
}
```

### Choosing Between `?` and `when`-`else`

Use `?` when you want to propagate the error unchanged. Use `when`-`else`
when you need to:

- Provide a custom error message
- Destructure something other than `Result` or `Option`
- Combine pattern and boolean guards in a flat sequence

```silt
-- Simple propagation: use ?
let value = parse(input)?

-- Custom error message: use when-else
when Ok(value) = parse(input) else {
  return Err("failed to parse input: expected integer")
}

-- Mixed pattern and boolean guards
fn process(input) {
  when Ok(value) = parse(input) else { return Err("parse failed") }
  when value > 0 else { return Err("must be positive") }
  Ok(value * 2)
}
```

### Never Type

`return` and `panic()` produce the `Never` type, which unifies with any
type. This lets them appear in any expression position:

```silt
fn get_or_die(opt) {
  match opt {
    Some(v) -> v
    None -> panic("expected a value")   -- Never unifies with v's type
  }
}
```

### Result and Option Utilities

```silt
result.map_ok(Ok(1), fn(x) { x + 1 })        -- Ok(2)
result.flat_map(Ok(1), fn(x) { Ok(x + 1) })   -- Ok(2)
result.unwrap_or(Err("x"), 0)                  -- 0

option.map(Some(1), fn(x) { x + 1 })          -- Some(2)
option.flat_map(Some(1), fn(x) { Some(x + 1) })  -- Some(2)
option.unwrap_or(None, 0)                      -- 0
```

`result.flat_map` is symmetric with `option.flat_map` -- both take a value
and a function that returns a wrapped result, and flatten the nesting.


## 8. Collections

### Lists

Ordered, homogeneous collections:

```silt
let numbers = [1, 2, 3, 4, 5]
```

Spread in list literals with `..`:

```silt
let full = [1, ..tail]
let merged = [..a, 3, ..b]
```

Key functions: `list.map`, `list.filter`, `list.fold`, `list.each`,
`list.find`, `list.zip`, `list.flatten`, `list.flat_map`, `list.filter_map`,
`list.sort_by`, `list.any`, `list.all`, `list.head`, `list.tail`,
`list.last`, `list.length`, `list.contains`, `list.append`, `list.concat`,
`list.reverse`, `list.get`, `list.take`, `list.drop`, `list.enumerate`,
`list.group_by`, `list.fold_until`, `list.unfold`.

### Maps

Unordered key-value collections with `#{ }`. Keys can be any hashable type:

```silt
let config = #{ "host": "localhost", "port": "8080" }
let grid = #{ (0, 0): "start", (1, 2): "end" }
```

Use `map.contains` to check key membership.

**Maps are homogeneous** -- all values must be the same type. This is
enforced by the type system. For heterogeneous data, use records:

```silt
-- ERROR: mixed String and Int values
let m = #{ "name": "Alice", "age": 30 }

-- OK: use a record
type Person { name: String, age: Int }
```

**Design rationale.** Heterogeneous maps defeat static typing. If the type
checker cannot know what `map.get(m, key)` returns, it cannot catch errors
at compile time.

Key functions: `map.get`, `map.set`, `map.delete`, `map.contains`,
`map.keys`, `map.values`, `map.entries`, `map.from_entries`, `map.length`,
`map.merge`, `map.filter`, `map.map`, `map.each`, `map.update`.

### Sets

Unordered unique-value collections with `#[ ]`:

```silt
let tags = #[1, 2, 3]
let words = #["hello", "world", "hello"]   -- duplicates removed
```

Set equality with `==`/`!=` works:

```silt
#[1, 2, 3] == #[3, 2, 1]   -- true
```

Key functions: `set.new`, `set.from_list`, `set.to_list`, `set.contains`,
`set.insert`, `set.remove`, `set.length`, `set.union`, `set.intersection`,
`set.difference`, `set.is_subset`, `set.map`, `set.filter`, `set.each`,
`set.fold`.


## 9. Loop Expression

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


## 10. Traits

Traits define shared behavior. No inheritance, no subclassing, no associated
types -- just methods.

### Declaration and Implementation

```silt
trait Display {
  fn display(self) -> String
}

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle(r={r})"
      Rect(w, h) -> "Rect({w}x{h})"
    }
  }
}

Circle(5.0).display()   -- "Circle(r=5)"
```

### Self Type

Use `Self` in trait method signatures to refer to the implementing type:

```silt
trait Monoid {
  fn empty() -> Self
  fn combine(a: Self, b: Self) -> Self
}

trait Monoid for Int {
  fn empty() -> Self { 0 }
  fn combine(a: Self, b: Self) -> Self { a + b }
}
```

### Built-in Traits

| Trait     | Purpose                          |
|-----------|----------------------------------|
| `Display` | Convert to human-readable string |
| `Equal`   | Equality comparison              |
| `Hash`    | Hash value for maps/sets         |
| `Compare` | Order comparison                 |

All four are **automatically derived** for every user-defined type. The
auto-derived `Display` formats in constructor syntax (`Circle(5)`). Write
your own `trait Display for T` to override.

### Where Clauses

Constrain generic parameters to types implementing a trait. Where clauses
**must** use explicit type annotations:

```silt
-- CORRECT: 'a' appears in the parameter annotation
fn print_all(items: List(a)) where a: Display {
  items |> list.each { item -> println(item.display()) }
}

-- ERROR: 'a' is unbound -- no annotation on x
fn f(x) where a: Display {
  println(x.display())
}
```

The form `fn f(x) where a: Display` is an error because the compiler cannot
determine which parameter `a` refers to.

Multiple trait bounds use `+`:

```silt
fn dedup(xs: List(a)) -> List(a) where a: Equal + Hash {
  ...
}
```

This is equivalent to `where a: Equal, a: Hash`.


## 11. Modules

**File = module.** Each `.silt` file is a module named after the file:

```silt
-- File: math.silt
pub fn add(a, b) = a + b
fn internal_helper(x) = x * 2   -- private
```

**Private by default.** Only `pub` items are exported. When a `pub type` has
enum variants, all constructors are exported too.

**Three import forms:**

```silt
import math                  -- qualified: math.add(1, 2)
import math.{ add, Point }   -- direct: add(1, 2)
import math as m              -- aliased: m.add(1, 2)
```

**Built-in modules** are registered in the global environment (no `.silt`
files needed):

| Module    | Key Functions                                                |
|-----------|--------------------------------------------------------------|
| `io`      | `inspect`, `read_file`, `write_file`, `read_line`, `args`   |
| `list`    | `map`, `filter`, `fold`, `each`, `find`, `zip`, ...         |
| `map`     | `get`, `set`, `delete`, `contains`, `keys`, `values`, ...   |
| `set`     | `new`, `from_list`, `to_list`, `contains`, `insert`, ...    |
| `string`  | `split`, `join`, `trim`, `contains`, `replace`, `length`    |
| `int`     | `parse`, `abs`, `min`, `max`, `to_float`, `to_string`       |
| `float`   | `parse`, `round`, `ceil`, `floor`, `abs`, `to_int`, `to_string` |
| `result`  | `map_ok`, `map_err`, `unwrap_or`, `flatten`, `flat_map`     |
| `option`  | `map`, `unwrap_or`, `to_result`, `flat_map`                 |
| `regex`   | `is_match`, `find`, `find_all`, `replace`, `replace_all_with` |
| `json`    | `parse`, `stringify`, `pretty`                               |
| `test`    | `assert`, `assert_eq`, `assert_ne`                           |
| `channel` | `new`, `send`, `receive`, `close`, `select`, `each`         |
| `task`    | `spawn`, `join`, `cancel`                                    |
| `time`    | `now`, `today`, `date`, `format`, `parse`, `add_days`, `weekday`, `sleep` |
| `http`    | `get`, `request`, `serve`, `segments`                        |

### Notable Standard Library Details

- `float.round`, `float.ceil`, `float.floor` return **`Float`**, not `Int`.
  Use `float.to_int` to convert after rounding.
- `float.to_string(f, decimals)` takes **two arguments** -- no overloading.
- `string.is_empty(s)` checks for zero-length strings.
- Character classification: `string.is_alpha`, `string.is_digit`,
  `string.is_upper`, `string.is_lower`, `string.is_alnum`,
  `string.is_whitespace`.
- `regex.replace_all_with(pattern, text, fn)` takes a callback for per-match
  replacement.
- The `time` module provides `Instant`, `Date`, `Time`, `DateTime`, `Duration`,
  and `Weekday` types with nanosecond precision. Comparison operators (`<`, `>`)
  work correctly on all time types. Display shows ISO 8601 format.
- The `http` module provides `Method` (enum: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`,
  `HEAD`, `OPTIONS`), `Request`, and `Response` record types. `http.get` and
  `http.request` return `Result(Response, String)`. `http.serve` takes a port
  and a handler function `Fn(Request) -> Response`. Pattern matching on
  `(req.method, segments)` replaces routing DSLs.

Circular imports are detected and rejected with a clear error message.


## 12. Concurrency

Silt uses CSP (Communicating Sequential Processes). Tasks communicate through
typed channels. No `async`/`await`, no colored functions.

```silt
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, "hello")
    channel.send(ch, "from")
    channel.send(ch, "silt")
    channel.close(ch)
  })

  let consumer = task.spawn(fn() {
    let Message(msg1) = channel.receive(ch)
    let Message(msg2) = channel.receive(ch)
    let Message(msg3) = channel.receive(ch)
    println("{msg1} {msg2} {msg3}")
  })

  task.join(producer)
  task.join(consumer)
}
```

`channel.receive` returns `Message(value)`, `Closed`, or `Empty` (for
`try_receive`). Use `^` pin to identify channels in `channel.select`:

```silt
match channel.select([urgent, normal]) {
  (^urgent, Message(msg)) -> println("Urgent: {msg}")
  (^normal, Message(msg)) -> println("Normal: {msg}")
  (_, Closed) -> println("Channel closed")
  _ -> println("No message")
}
```

**Fan-out pattern:** spawn multiple workers, collect results:

```silt
fn main() {
  let results = channel.new(10)

  let workers = [1, 2, 3] |> list.map { id ->
    task.spawn(fn() {
      channel.send(results, id * 10)
    })
  }

  workers |> list.each { w -> task.join(w) }
  channel.close(results)

  let Message(r1) = channel.receive(results)
  let Message(r2) = channel.receive(results)
  let Message(r3) = channel.receive(results)
  println("results: {r1}, {r2}, {r3}")
}
```

Tasks are lightweight and run in parallel on a fixed thread pool. Spawning a
task is cheap, and you can have thousands running concurrently. Channels
coordinate between them. Any function can spawn, send, or receive — there is
no function coloring. I/O operations like `io.read_file` and `http.get`
transparently yield to the scheduler when called inside a spawned task, so
they never block the thread pool.

For the full treatment, see [concurrency.md](concurrency.md).


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

## Ranges

The `..` operator creates an inclusive range. `1..10` includes both 1 and 10.
Ranges are lazy — they don't allocate memory until iterated, so `1..1000000`
is cheap. All `list.*` functions work on ranges directly.

```silt
1..10
|> list.map { n -> n * n }
|> list.each { n -> println("{n}") }
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


# Part 3: Design Trade-offs

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


# Putting It All Together

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

FizzBuzz:

```silt
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}

fn main() {
  1..100
  |> list.map { n -> fizzbuzz(n) }
  |> list.each { s -> println(s) }
}
```

Graph search with `loop`:

```silt
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

fn main() {
  let graph = #{
    1: [2, 3], 2: [1, 4, 5], 3: [1, 5],
    4: [2], 5: [2, 3, 6], 6: [5]
  }

  match bfs(graph, 1, 6) {
    Some(n) -> println("found node {n}")
    None -> println("not reachable")
  }
}
```
