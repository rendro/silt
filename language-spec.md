# Silt Language Spec — Draft v0.2

> A minimal, statically-typed, expression-based language with CSP concurrency.
> 13 keywords. Fully immutable. Pattern matching as the sole branching construct.
> Implemented in Rust. File extension: `.silt`

-----

## Keywords (13)

```
let  fn  type  trait  match  when  return
pub  mod  import  as  else  where
```

`_` is a wildcard pattern, not a keyword.
`true`, `false` are builtin literals, not keywords.
`Ok`, `Err`, `Some`, `None` are builtin variant constructors.

-----

## 1. Bindings

All bindings are immutable. Rebinding (shadowing) is allowed.

```
let x = 42
let name = "Robert"
let x = x + 1          -- shadowing, not mutation
```

Type annotations are optional everywhere — HM inference handles it.

```
let x: Int = 42        -- explicit (rarely needed)
```

-----

## 2. Functions

All functions are expressions. Last expression is the return value.

```
fn add(a, b) {
  a + b
}

-- anonymous
let double = fn(x) { x * 2 }

-- single-expression shorthand
fn square(x) = x * x
```

### Function signatures (inferred, but expressible)

```
fn add(a: Int, b: Int) -> Int {
  a + b
}
```

### Trailing closures

When the last argument to a function is a closure, it can be written
outside the parentheses with a lightweight `{ args -> body }` syntax.

```
-- these are equivalent
[1, 2, 3] |> map(fn(x) { x * 2 })
[1, 2, 3] |> map { x -> x * 2 }

-- multi-line
users |> filter { user ->
  user.age > 18 && user.active
}

-- multiple args
[1, 2, 3] |> fold(0) { acc, x -> acc + x }
```

-----

## 3. Types

### Algebraic types (tagged unions)

```
type Option(a) {
  Some(a)
  None
}

type Result(a, e) {
  Ok(a)
  Err(e)
}

type Shape {
  Circle(Float)
  Rect(Float, Float)
}
```

### Records

```
type User {
  name: String,
  age: Int,
  email: String,
}
```

Record creation and access:

```
let u = User { name: "Alice", age: 30, email: "a@b.com" }
let n = u.name
```

Record update (returns new record):

```
let u2 = u.{ age: 31 }
let u3 = u.{ age: 31, email: "new@b.com" }
```

Reads as “u, but with these fields.” No keyword cost, no sigil overload.

### Tuples

```
let pair = (1, "hello")
let (x, y) = pair       -- destructuring
```

-----

## 4. Traits

Lightweight interfaces for ad-hoc polymorphism. No inheritance, no associated types.

```
trait Display {
  fn display(self) -> String
}

trait Compare {
  fn compare(self, other: Self) -> Ordering
}
```

Implementing traits for a type:

```
trait Display for User {
  fn display(self) -> String {
    "User({self.name}, age {self.age})"
  }
}
```

Using traits as constraints in generics:

```
fn print_all(items: List(a)) where a: Display {
  items |> each { item -> print(item.display()) }
}
```

Built-in traits: `Display`, `Compare`, `Equal`, `Hash`.

-----

## 5. Pattern Matching

The sole branching mechanism. Exhaustiveness is checked at compile time.

```
fn describe(shape) {
  match shape {
    Circle(r) -> "circle with radius {r}"
    Rect(w, h) -> "rect {w}x{h}"
  }
}
```

### Guards in match arms

```
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}
```

### Nested / deep matching

```
match result {
  Ok(Some(value)) -> use(value)
  Ok(None) -> handle_empty()
  Err(e) -> handle_error(e)
}
```

### Pin operator (`^`)

The `^` prefix in a pattern matches against the current value of an existing
variable instead of creating a new binding. Without `^`, a name in a pattern
always introduces a fresh binding that shadows any outer variable.

```
let expected = 42
match input {
  ^expected -> "got the expected value"
  other -> "got {other} instead"
}
```

The pin operator works in any pattern position -- tuples, constructors, lists,
and nested patterns:

```
let target = "hello"
match messages {
  [(^target, data), ..rest] -> handle(data)
  _ -> skip()
}
```

This is especially useful with `channel.select`, where you need to identify
which channel produced a value:

```
match channel.select([ch1, ch2]) {
  (^ch1, msg) -> handle1(msg)
  (^ch2, msg) -> handle2(msg)
  _ -> panic("unexpected")
}
```

### Let-match (destructuring bind)

```
let (x, y, _) = triple
let User { name, age, .. } = user     -- partial record destructure
```

### Guard Statement (`when`)

`when` in a function body asserts a pattern and binds/narrows on success.
The `else` branch **must diverge** (return, panic).

```
fn process(input) {
  -- assert + destructure, bail on failure
  when Ok(value) = parse(input) else {
    return Err("parse failed")
  }

  -- value is bound here, type-narrowed
  when Some(user) = find_user(value) else {
    return Err("not found")
  }

  -- narrow on variant
  when Admin(perms) = user.role else {
    return Err("unauthorized")
  }

  -- all bindings available, fully narrowed
  do_admin_thing(user, perms)
}
```

Use `?` for simple Result/Option propagation, `when` for custom
error handling, destructuring, or type narrowing beyond Result.

-----

## 6. Pipe Operator

```
let result =
  [1, 2, 3, 4, 5]
  |> filter { x -> x > 2 }
  |> map { x -> x * 10 }
  |> fold(0) { acc, x -> acc + x }
```

Pipes pass the left side as the **first argument** to the right side.

-----

## 7. String Interpolation

```
let greeting = "hello {name}, you are {age} years old"
let debug = "result: {inspect(value)}"
```

Curly braces inside strings evaluate expressions. Escaping: `\{`.
Interpolated values must implement `Display`.

-----

## 8. Concurrency (CSP)

All concurrency primitives are module-qualified: channels live in the `channel`
module, tasks live in the `task` module. There are no concurrency keywords.

### Channels

```
let ch = channel.new()          -- unbuffered channel, type inferred
let ch = channel.new(10)        -- buffered with capacity 10
```

### Send / Receive

```
channel.send(ch, "hello")           -- blocks if unbuffered and no receiver
let msg = channel.receive(ch)       -- blocks until message available
```

### Spawn

```
let handle = task.spawn(fn() {
  let result = do_work()
  channel.send(ch, result)
})
```

`task.spawn` takes a zero-arg function, runs it concurrently, returns a `Handle(a)`.

### Handles

```
let result = task.join(handle)      -- blocks until task completes, returns Result
task.cancel(handle)                 -- request cancellation
```

### Select

`channel.select` waits on multiple channels and returns a `(channel, value)` tuple
for whichever channel has data first. Use the `^` pin operator to match on
which channel produced the value:

```
match channel.select([ch1, ch2]) {
  (^ch1, msg) -> handle_a(msg)
  (^ch2, msg) -> handle_b(msg)
  _ -> panic("unexpected")
}
```

-----

## 9. Modules & Visibility

```
-- file: math.silt

pub fn add(a, b) = a + b
fn internal_helper(x) = x * 2    -- private by default
pub type Point { x: Float, y: Float }
```

```
-- file: main.silt

import math
import math.{ add, Point }
import math as m
```

Everything is private by default. `pub` exports.

-----

## 10. Error Handling

No exceptions. `Result` and `Option` are the only error mechanisms.

```
fn parse_int(s: String) -> Result(Int, String) {
  -- ...
}

fn main() {
  let input = "42"

  -- explicit matching
  match parse_int(input) {
    Ok(n) -> "got {n}"
    Err(e) -> "failed: {e}"
  }

  -- pipe-friendly with trailing closures
  parse_int(input)
  |> map_ok { n -> n * 2 }
  |> unwrap_or(0)
}
```

### The `?` operator (sugar for early return on Err/None)

```
fn process(input) {
  let n = parse_int(input)?
  let result = validate(n)?
  Ok(result * 2)
}
```

This desugars to match + early return of the error variant.

-----

## 11. Builtins

### Primitive types

`Int`, `Float`, `String`, `Bool`, `List(a)`, `Map(k, v)`, `Option(a)`, `Result(a, e)`

### Operators

```
+  -  *  /  %              -- arithmetic
==  !=  <  >  <=  >=       -- comparison
&&  ||  !                  -- boolean
|>                         -- pipe
?                          -- error propagation
..                         -- range (1..10)
^                          -- pin (in patterns)
```

### List / Map literals

```
let xs = [1, 2, 3]
let m = #{ "key": "value", "count": 42 }
```

-----

## 12. Standard Library (v1)

### Builtin (always available, no import)

`print`, `println`, `inspect`, `panic`

### Stdlib modules (import to use)

|Module   |Provides                                   |
|---------|-------------------------------------------|
|`io`     |read_file, write_file, read_line, args     |
|`list`   |map, filter, fold, each, find, zip, flatten|
|`map`    |get, set, delete, keys, values, merge      |
|`string` |split, join, trim, contains, replace       |
|`int`    |parse, abs, min, max, to_float             |
|`float`  |parse, round, ceil, floor                  |
|`result` |map_ok, map_err, unwrap_or, flatten        |
|`option` |map, unwrap_or, to_result                  |
|`test`   |assert, assert_eq, assert_ne, run          |
|`channel`|new, send, receive, close, select, try_send, try_receive|

### Not in v1 (future)

JSON, HTTP, networking, package manager, FFI

### I/O examples

```
-- print is builtin, no import needed
println("hello world")

-- file I/O returns Result
import io

fn main() {
  let content = io.read_file("data.txt")?
  let lines = content |> string.split("\n")
  lines |> each { line -> println(line) }

  io.write_file("out.txt", "done")?
}

-- stdin
let name = io.read_line()?
println("hello {name}")

-- CLI args
let args = io.args()
```

-----

## 13. Comments

```
-- single line comment

{-
   block comment
   can be nested {- like this -}
-}
```

-----

## 14. Testing

Built-in test framework. Tests are functions annotated with a naming convention.

```
-- file: math_test.silt

import math
import test.{ assert_eq }

fn test_add() {
  assert_eq(math.add(1, 2), 3)
  assert_eq(math.add(-1, 1), 0)
}

fn test_square() {
  assert_eq(math.square(5), 25)
}
```

Run with `silt test` or `silt test math_test.silt`.

-----

## Summary

|Aspect         |Choice                                    |
|---------------|------------------------------------------|
|Keywords       |13                                        |
|Branching      |`match` only + `when` guard statement     |
|Types          |HM inference, algebraic + records + traits|
|Mutability     |None (rebinding/shadowing ok)             |
|Iteration      |`                                         |
|Errors         |`Result`/`Option` + `?` operator          |
|Concurrency    |`task.spawn`, typed `channel.new`, `channel.select`, handles|
|Data structures|Records, tuples, List, Map                |
|Strings        |Interpolation with `{expr}` via Display   |
|Visibility     |Private default, `pub` to export          |
|Traits         |Lightweight interfaces, no inheritance    |
|Testing        |Built-in, convention-based                |
|Implementation |Rust, tree-walk interpreter (v1)          |

-----

## Future Work

1. **Package management / dependencies**
1. **REPL**
1. **Bytecode VM** (v2 — performance)
1. **FFI** (interop with Rust/C)
1. **JSON / HTTP stdlib modules**
1. **LSP / editor support** (Zed, VSCode)
