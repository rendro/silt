# Silt Language Guide

Silt is a statically-typed, expression-based programming language with full immutability,
pattern matching as the sole branching construct, and CSP-style concurrency. It compiles
to a tree-walk interpreter (v1) written in Rust. File extension: `.silt`.

The language has 14 keywords and only 13 global names (`print`, `println`, `panic`, `try`,
`Ok`, `Err`, `Some`, `None`, `Stop`, `Continue`, `Message`, `Closed`, `Empty`). Everything
else is module-qualified (e.g. `list.map`, `string.split`, `channel.new`, `task.spawn`).

This guide walks through every major feature of the language, explains the design
decisions behind it, and gives you enough examples to start writing real programs.

---

## 1. Bindings & Immutability

### `let` bindings

Every value in Silt is bound with `let`. There is no `var`, no `mut`, no reassignment.
Once a name is bound, that binding is permanent.

```silt
let x = 42
let name = "Robert"
let active = true
```

### Shadowing

You cannot mutate a binding, but you can *shadow* it -- create a new binding with the
same name. The old value is not modified; a new value simply takes its place in scope.

```silt
let x = 1
let x = x + 1   -- x is now 2; the original 1 is untouched
let x = x * 3   -- x is now 6
```

This looks like mutation but is fundamentally different. Each `let` creates a fresh
binding. The compiler can reason about each independently.

### Why immutability?

Silt is immutable by design. Every value, every binding, every data structure -- none of
them change after creation. This is not a restriction; it is a simplification.

- **Easier reasoning.** If a value cannot change, you never need to ask "who changed this
  and when?" You look at where it was created and you know what it is.
- **No race conditions.** Silt has concurrent tasks communicating over channels. Because
  nothing is mutable, there are no data races. No locks. No atomics. No defensive copies.
- **Works naturally with the language.** Pattern matching, pipelines, and record updates
  all produce new values. Mutation would fight the design rather than help it.

### Type annotations

Type annotations are optional. Silt uses Hindley-Milner type inference, so the compiler
can figure out types for you. But you can annotate when you want to be explicit.

```silt
let x: Int = 42
let name: String = "Alice"
let ratio: Float = 3.14
let transform: Fn(Int) -> Int = fn(x) { x * 2 }
```

You can annotate function types with `Fn(params) -> Return`:

```silt
let apply: Fn(Int, Int) -> Int = fn(a, b) { a + b }
let callbacks: List(Fn(String) -> String) = [string.to_upper, string.to_lower]
```

Function type annotations work in record fields too:

```silt
type Handler {
  name: String,
  run: Fn(String) -> String,
}
```

Annotations are most useful in function signatures and public APIs, where they serve as
documentation for humans as much as instructions for the compiler.

---

## 2. Primitive Types

Silt has five primitive types.

| Type     | Description                          | Examples                    |
|----------|--------------------------------------|-----------------------------|
| `Int`    | 64-bit signed integer                | `42`, `-7`, `0`             |
| `Float`  | 64-bit floating-point                | `3.14`, `-0.5`, `1.0`      |
| `Bool`   | Boolean                              | `true`, `false`             |
| `String` | UTF-8 string with interpolation      | `"hello"`, `"age: {n}"`, `"""raw"""` |
| `Unit`   | The type with no meaningful value    | (returned by `println` etc.)|

### Arithmetic operators

```silt
let sum = 10 + 3       -- 13
let diff = 10 - 3      -- 7
let prod = 10 * 3      -- 30
let quot = 10 / 3      -- 3 (integer division)
let rem = 10 % 3       -- 1
```

Arithmetic works on both `Int` and `Float`. Integer division truncates.

```silt
let pi = 3.14159
let area = pi * 5.0 * 5.0   -- 78.53975
```

### Comparison operators

```silt
let eq = 1 == 1        -- true
let neq = 1 != 2       -- true
let lt = 3 < 5         -- true
let gt = 5 > 3         -- true
let leq = 3 <= 3       -- true
let geq = 5 >= 5       -- true
```

### Boolean operators

```silt
let both = true && false     -- false
let either = true || false   -- true
let negated = !true          -- false
```

### Ranges

The `..` operator creates a range. Ranges are commonly used with pipes and
collection functions.

```silt
1..10          -- range from 1 to 10
1..101         -- range from 1 to 101
```

A classic use is generating sequences for processing:

```silt
fn main() {
  1..11
  |> list.map { n -> n * n }
  |> list.each { n -> println("{n}") }
}
```

---

## 3. Functions

Functions are the primary building block of Silt programs. They are expressions -- every
function produces a value, and the last expression in the body is the return value.

### Named functions

```silt
fn add(a, b) {
  a + b
}
```

The body is a block delimited by braces. The last expression (`a + b`) is the return
value. No `return` keyword needed.

### Single-expression shorthand

When a function body is a single expression, use `=` instead of braces:

```silt
fn square(x) = x * x
fn double(x) = x * 2
fn greet(name) = "hello {name}"
```

This is not sugar for something else -- it is the natural form for small functions.

### Anonymous functions (lambdas)

Functions are values. You can create unnamed functions and bind them to variables:

```silt
let double = fn(x) { x * 2 }
let add = fn(a, b) { a + b }
```

Anonymous functions close over their environment:

```silt
fn make_adder(n) {
  fn(x) { x + n }
}

fn main() {
  let add5 = make_adder(5)
  add5(10)   -- 15
}
```

### Return type annotations

You can annotate parameter types and the return type:

```silt
fn add(a: Int, b: Int) -> Int {
  a + b
}

fn birthday(user: User) -> User {
  user.{ age: user.age + 1 }
}
```

Type annotations are always optional but improve readability, especially for public
functions.

### Early return with `return`

The last expression is the implicit return value, but you can use `return` for early
exit. This is most common in error-handling paths.

```silt
fn process(input) {
  when Ok(value) = parse(input) else {
    return Err("parse failed")
  }

  Ok(value * 2)
}
```

### Expression-based design

Because everything is an expression, functions naturally compose. There is no
distinction between "statements that do things" and "expressions that produce values."
A `match` is an expression. A block is an expression. A function call is an expression.
This means you can use any of them anywhere a value is expected.

---

## 4. Collections

Silt has three built-in collection types: lists, tuples, and maps.

### Lists

Lists are ordered, homogeneous collections.

```silt
let numbers = [1, 2, 3, 4, 5]
let names = ["Alice", "Bob", "Carol"]
let empty = []
```

Lists work with the `list` module functions via pipes:

```silt
fn main() {
  [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
  -- result: 120
}
```

Since Silt is immutable, list operations always produce new lists. The original is never
modified.

#### Spread in list literals

Use `..` to spread one list into another. This works in any position and you can
use multiple spreads:

```silt
let tail = [2, 3, 4]
let full = [1, ..tail]             -- [1, 2, 3, 4]

let head = [1, 2]
let extended = [..head, 3, 4]      -- [1, 2, 3, 4]

let a = [1, 2]
let b = [4, 5]
let merged = [..a, 3, ..b]         -- [1, 2, 3, 4, 5]
```

### Tuples

Tuples are fixed-size, heterogeneous collections. They group values of different types
together.

```silt
let pair = (1, "hello")
let triple = (true, 42, "world")
```

Destructure tuples with `let`:

```silt
let (x, y) = pair         -- x = 1, y = "hello"
let (_, _, z) = triple    -- z = "world", first two ignored
```

Tuples are excellent for returning multiple values from a function:

```silt
fn min_max(xs) {
  let min = xs |> list.fold(999) { acc, x -> match x < acc { true -> x, _ -> acc } }
  let max = xs |> list.fold(0) { acc, x -> match x > acc { true -> x, _ -> acc } }
  (min, max)
}

fn main() {
  let (lo, hi) = min_max([3, 1, 4, 1, 5, 9])
  println("min={lo}, max={hi}")
}
```

### Maps

Maps are unordered key-value collections, written with the `#{ }` syntax.
Keys can be any hashable type — strings, ints, bools, tuples, enums, or records:

```silt
let config = #{ "host": "localhost", "port": "8080" }
let counts = #{ "apples": 3, "bananas": 5 }
let grid = #{ (0, 0): "start", (1, 2): "end" }
```

Maps are useful for dynamic lookups and configuration data.

```silt
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  println(m)
}
```

#### Maps are homogeneous

All values in a map must be the same type. This is enforced by the type system:

```silt
let m = #{ "name": "Alice", "age": 30 }  -- ERROR: mixed String and Int values
```

For heterogeneous data, use records instead -- each field has its own type:

```silt
type Person { name: String, age: Int }
let person = Person { name: "Alice", age: 30 }   -- OK
```

For dynamic string-keyed data where values differ, convert to a common type:

```silt
let summary = #{ "name": "Engineering", "count": int.to_string(4) }
```

---

## 5. Algebraic Types

Silt's type system is built on algebraic types: enums (tagged unions) and records
(product types). Together, they let you model your domain precisely -- making illegal
states unrepresentable.

### Enums (tagged unions)

An enum defines a type that can be one of several variants, each optionally carrying
data:

```silt
type Shape {
  Circle(Float)
  Rect(Float, Float)
}
```

`Shape` is a type. `Circle` and `Rect` are constructors. You use them to create values:

```silt
let c = Circle(5.0)
let r = Rect(3.0, 4.0)
```

Enums are the backbone of pattern matching. Because every variant is explicit, the
compiler can check that you handle all cases.

### Generic types

Types can be parameterized. Silt's built-in `Option` and `Result` are defined this way:

```silt
type Option(a) {
  Some(a)
  None
}

type Result(a, e) {
  Ok(a)
  Err(e)
}
```

The `a` and `e` are type parameters -- they get filled in with concrete types when used.
`Option(Int)`, `Result(String, String)`, and so on.

### Records

Records are types with named fields:

```silt
type User {
  name: String,
  age: Int,
  active: Bool,
}
```

### Record creation

Create a record by specifying all fields:

```silt
let alice = User { name: "Alice", age: 30, active: true }
let bob = User { name: "Bob", age: 25, active: false }
```

### Field access

Use dot notation to access fields:

```silt
let name = alice.name     -- "Alice"
let age = alice.age       -- 30
```

### Record update

Silt's record update syntax creates a new record with some fields changed. The original
is untouched (immutability).

```silt
let alice2 = alice.{ age: 31 }
let alice3 = alice.{ age: 31, active: false }
```

Read `alice.{ age: 31 }` as **"alice, but with age 31."** The `.{ }` syntax was chosen
to read naturally: the dot connects to the record, the braces list the changes. No
keyword cost, no sigil overload -- just a clear visual cue that you are deriving a new
value from an existing one.

This is especially useful in functions that transform records:

```silt
fn birthday(user: User) -> User {
  user.{ age: user.age + 1 }
}

fn deactivate(user: User) -> User {
  user.{ active: false }
}
```

---

## 6. Pattern Matching

### `match` is the only branching construct

Silt has no `if`, no `else`, no ternary operator, no `switch`. There is only `match`.

This is deliberate. When every branch in your program uses the same construct, there is
one thing to learn, one thing to read, and one thing the compiler checks. The compiler
verifies exhaustiveness: if you forget a case, it tells you at compile time.

```silt
fn describe(shape) {
  match shape {
    Circle(r) -> "circle with radius {r}"
    Rect(w, h) -> "rect {w}x{h}"
  }
}
```

### Literal patterns

Match against concrete values:

```silt
fn to_word(n) {
  match n {
    0 -> "zero"
    1 -> "one"
    2 -> "two"
    _ -> "many"
  }
}
```

### Variable binding

A bare name in a pattern binds the matched value to that name:

```silt
fn describe(opt) {
  match opt {
    Some(value) -> "got {value}"
    None -> "nothing"
  }
}
```

Here, `value` is bound to whatever is inside `Some`.

### Wildcard `_`

The underscore matches anything and discards it:

```silt
fn is_ok(result) {
  match result {
    Ok(_) -> true
    Err(_) -> false
  }
}
```

### Constructor patterns

Destructure enum variants:

```silt
fn area(shape) {
  match shape {
    Circle(r) -> 3.14159 * r * r
    Rect(w, h) -> w * h
  }
}
```

### Tuple patterns

Match against tuples by position. This is the classic FizzBuzz:

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

By matching on `(n % 3, n % 5)` as a tuple, the logic becomes a clean table of cases
rather than nested conditionals.

### Record destructuring

Pull out fields by name. Use `..` to ignore the rest:

```silt
let User { name, age, .. } = user

-- Use in match arms
fn greet(user) {
  match user {
    User { name, active: true, .. } -> "hello {name}"
    User { name, .. } -> "{name} is inactive"
  }
}
```

### Nested patterns

Patterns compose. Match through multiple layers of structure at once:

```silt
fn handle(result) {
  match result {
    Ok(Some(value)) -> use(value)
    Ok(None) -> handle_empty()
    Err(e) -> handle_error(e)
  }
}
```

This eliminates the nested `match` blocks you would otherwise need.

### Guards

Add conditions to match arms with `when`:

```silt
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}
```

The guard `when x > 0` is checked only after the pattern matches. If the guard fails,
matching continues to the next arm.

### Guardless match

When you want to branch on boolean conditions without a scrutinee value, use a
guardless match. Omit the scrutinee entirely:

```silt
fn classify(n) {
  match {
    n == 0 -> "zero"
    n > 0  -> "positive"
    _      -> "negative"
  }
}
```

Each arm's left-hand side is a boolean expression (or `_` for the default case).
The first arm whose condition is true is taken. This is a convenient alternative
to matching on `true`/`false` for simple conditional logic.

### Or-patterns

Or-patterns let you match multiple alternatives in a single arm using `|`:

```silt
fn describe(n) {
  match n {
    0 | 1 -> "small"
    2 | 3 -> "medium"
    _ -> "large"
  }
}
```

### Range patterns

Range patterns match a value against an inclusive numeric range using `..`:

```silt
fn grade(score) {
  match score {
    90..100 -> "A"
    80..89  -> "B"
    70..79  -> "C"
    _       -> "F"
  }
}
```

### Map patterns

Map patterns destructure maps by key, binding values to names:

```silt
fn greet(config) {
  match config {
    #{ "name": name, "greeting": greeting } -> "{greeting}, {name}!"
    #{ "name": name } -> "hello, {name}!"
    _ -> "hello, stranger!"
  }
}
```

### List patterns

Lists can be destructured in both `match` arms and `let` bindings using bracket
syntax. There are three forms:

**Empty list** -- matches a list with no elements:

```silt
match xs {
  [] -> "empty"
  _ -> "not empty"
}
```

**Exact length** -- matches a list with exactly the given number of elements and
binds each one:

```silt
let [a, b, c] = [1, 2, 3]
-- a = 1, b = 2, c = 3
```

Without `..`, the pattern requires an exact length match. `[a, b]` will only match
a list of exactly two elements.

**Head/tail destructuring** -- uses `..` to bind the remaining elements as a new
list:

```silt
match xs {
  [] -> "empty"
  [head, ..tail] -> "head is {head}, rest has {list.length(tail)} elements"
}
```

The `..` prefix binds all remaining elements into a new list. You can match any
number of leading elements before the rest:

```silt
match xs {
  [a, b, ..rest] -> "first two: {a}, {b}"
  _ -> "fewer than two elements"
}
```

**Nested patterns** -- list elements can contain any pattern, including constructor
patterns:

```silt
match xs {
  [Some(a), Some(b), ..rest] -> "first two are present: {a}, {b}"
  _ -> "something else"
}
```

List patterns are especially useful for recursive processing:

```silt
fn sum(xs) {
  match xs {
    [] -> 0
    [head, ..tail] -> head + sum(tail)
  }
}
```

The `..` syntax is consistent with record rest patterns (`User { name, .. }`),
though in list patterns the rest is always bound to a name rather than discarded.

### Pin operator (`^`)

The `^` prefix in a pattern matches against the current value of an existing variable
instead of creating a new binding. Normally, a name in a pattern introduces a fresh
binding that shadows any outer variable. The pin operator overrides this behavior.

```silt
let expected = 42
match input {
  ^expected -> "got the expected value"
  other -> "got {other} instead"
}
```

Without `^`, the pattern `expected` would just create a new binding (always matching
any value). With `^expected`, the match only succeeds if `input` equals the current
value of the `expected` variable.

The pin operator works in any pattern position -- tuples, constructors, lists, and
nested patterns:

```silt
let target = "hello"
match messages {
  [(^target, data), ..rest] -> handle(data)
  _ -> skip()
}
```

A common use is with `channel.select`, where you need to identify which channel
produced a value:

```silt
match channel.select([ch1, ch2]) {
  (^ch1, msg) -> handle_first(msg)
  (^ch2, msg) -> handle_second(msg)
  _ -> panic("unexpected")
}
```

### Exhaustiveness checking

The compiler checks that your match covers all possible cases. If you forget a variant,
you get a compile-time error rather than a runtime crash. This is one of the strongest
benefits of having `match` as the only branching construct -- the compiler can help you
because it knows the shape of every decision in your program.

---

## 7. The Pipe Operator

### Left-to-right data flow

The pipe operator `|>` passes the value on its left as the **first argument** to the
function on its right:

```silt
-- These are equivalent:
list.filter(xs, fn(x) { x > 0 })
xs |> list.filter { x -> x > 0 }
```

### Why pipes?

Without pipes, function composition nests inward:

```silt
-- Without pipes: read inside-out
list.each(list.map(list.filter([1, 2, 3, 4, 5], fn(x) { x > 2 }), fn(x) { x * 10 }), fn(x) { println("{x}") })
```

With pipes, the same code reads left-to-right, top-to-bottom:

```silt
-- With pipes: read top-to-bottom
[1, 2, 3, 4, 5]
|> list.filter { x -> x > 2 }
|> list.map { x -> x * 10 }
|> list.each { x -> println("{x}") }
```

Pipes eliminate nesting and make the data flow explicit. You can see at a glance what
happens to the data and in what order.

### Pipelines with trailing closures

Pipes combine naturally with trailing closures to form readable processing pipelines:

```silt
fn main() {
  let total =
    [1, 2, 3, 4, 5]
    |> list.filter { x -> x > 2 }
    |> list.map { x -> x * 10 }
    |> list.fold(0) { acc, x -> acc + x }

  println("total: {total}")   -- total: 120
}
```

### Real-world example

Here is a complete pipeline processing records:

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

---

## 8. Trailing Closures

When the last argument to a function is a closure, you can write it outside the
parentheses using the `{ args -> body }` syntax.

### Basic form

```silt
-- Standard call with anonymous function
[1, 2, 3] |> list.map(fn(x) { x * 2 })

-- Same thing with a trailing closure
[1, 2, 3] |> list.map { x -> x * 2 }
```

The trailing closure form is shorter and reads more naturally, especially in pipelines.

### Multi-line closures

Closures can span multiple lines. The body is everything after the `->`:

```silt
users |> list.filter { user ->
  user.age > 18 && user.active
}
```

### Multiple arguments

Trailing closures can take multiple parameters, separated by commas:

```silt
[1, 2, 3] |> list.fold(0) { acc, x -> acc + x }
```

Here, `list.fold` takes an initial value `0` in parentheses and the accumulator function as
a trailing closure. `acc` is the accumulator, `x` is the current element.

### Destructuring in closure parameters

You can destructure tuples directly in closure parameters:

```silt
let pairs = [(1, "one"), (2, "two"), (3, "three")]

pairs |> list.each { (n, word) ->
  println("{n} is {word}")
}
```

### Combining pipes and trailing closures

The pipe operator and trailing closures were designed to work together. This combination
is the idiomatic Silt style for data processing:

```silt
fn main() {
  let shapes = [Circle(5.0), Rect(3.0, 4.0), Circle(1.0)]

  shapes
  |> list.map { s -> (s.display(), area(s)) }
  |> list.each { pair -> println("{pair}") }
}
```

---

## 9. Error Handling

### No exceptions

Silt has no exceptions. No `try/catch`, no `throw`, no hidden control flow. Errors are
values, represented by two types: `Result` and `Option`.

This is a deliberate choice. Exceptions create invisible control flow paths -- any
function might throw, and the caller might not know. In Silt, if a function can fail, its
return type says so. If it returns `Result(User, String)`, you know it can fail, and the
compiler ensures you handle both cases.

### `Result(a, e)`

A `Result` represents an operation that can succeed or fail:

```silt
type Result(a, e) {
  Ok(a)       -- success, carrying a value
  Err(e)      -- failure, carrying an error
}
```

```silt
fn parse_int(s: String) -> Result(Int, String) {
  -- returns Ok(n) on success, Err(message) on failure
}
```

Handle results with `match`:

```silt
match parse_int("42") {
  Ok(n) -> println("got {n}")
  Err(e) -> println("failed: {e}")
}
```

### `Option(a)`

An `Option` represents a value that might not exist:

```silt
type Option(a) {
  Some(a)     -- a value is present
  None        -- no value
}
```

```silt
fn find_user(id) {
  -- returns Some(user) if found, None otherwise
}
```

### The `?` operator

The `?` operator is sugar for "if this is an error, return it immediately." It removes
the boilerplate of matching on every `Result` or `Option`.

```silt
fn process(input) {
  let n = parse_int(input)?       -- returns Err early if parse fails
  let result = validate(n)?       -- returns Err early if validation fails
  Ok(result * 2)
}
```

Without `?`, you would need to match each result:

```silt
fn process(input) {
  let n = match parse_int(input) {
    Ok(val) -> val
    Err(e) -> return Err(e)
  }
  let result = match validate(n) {
    Ok(val) -> val
    Err(e) -> return Err(e)
  }
  Ok(result * 2)
}
```

The `?` operator keeps the "happy path" clean while ensuring errors are always handled.

### `when`-`else` guards

For more complex error handling -- when you need custom error messages, destructuring,
or type narrowing -- use `when`-`else`:

```silt
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  when Some(port_line) = lines |> list.find { l -> string.contains(l, "port=") } else {
    return Err("missing port in config")
  }

  let host = host_line |> string.replace("host=", "")
  let port_result = port_line |> string.replace("port=", "") |> int.parse()
  when Ok(port) = port_result else {
    return Err("invalid port number")
  }

  Ok("connecting to {host}:{port}")
}
```

The `when` statement asserts a pattern and binds on success. The `else` branch **must
diverge** -- it must `return` or `panic`. After the `when`, the bound variables are
available in the rest of the function.

This gives you `?`-like early returns but with custom error messages and the ability to
destructure any pattern, not just `Result` and `Option`.

### Choosing between `?` and `when`-`else`

Use `?` when you simply want to propagate the error as-is. Use `when`-`else` when you
need to:

- Provide a custom error message
- Destructure something other than `Result` or `Option`
- Narrow a type based on a variant

```silt
-- Simple propagation: use ?
let value = parse(input)?

-- Custom error: use when-else (pattern form)
when Ok(value) = parse(input) else {
  return Err("failed to parse input: expected integer")
}
```

`when`-`else` also accepts boolean expressions. If the condition is true, execution
continues. If false, the else block runs (which must diverge via `return` or `panic`):

```silt
-- Boolean guard: use when-else (boolean form)
fn buy(qty, balance, price) {
  when qty > 0 else { return Err("out of stock") }
  when balance >= price else { return Err("not enough money") }
  Ok("purchased")
}
```

Both forms can be mixed freely in the same function:

```silt
fn process(input) {
  when Ok(value) = parse(input) else { return Err("parse failed") }
  when value > 0 else { return Err("must be positive") }
  Ok(value * 2)
}
```

---

## 10. Traits

Traits define shared behavior that types can implement. They are Silt's mechanism for
ad-hoc polymorphism -- the same function name can do different things depending on the
type. There is no inheritance, no subclassing, no associated types. Just methods.

### Declaring a trait

```silt
trait Display {
  fn display(self) -> String
}

trait Compare {
  fn compare(self, other: Self) -> Ordering
}
```

The `self` parameter means the method is called on a value of the implementing type.

### Implementing a trait

Use `trait TraitName for TypeName` to implement:

```silt
type Shape {
  Circle(Float)
  Rect(Float, Float)
}

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle(r={r})"
      Rect(w, h) -> "Rect({w}x{h})"
    }
  }
}
```

### Method calls

Once a trait is implemented, call its methods with dot notation:

```silt
let c = Circle(5.0)
println(c.display())     -- "Circle(r=5)"

let r = Rect(3.0, 4.0)
println(r.display())     -- "Rect(3x4)"
```

### Where constraints

Use `where` to constrain generic type parameters to types that implement a trait:

```silt
fn print_all(items: List(a)) where a: Display {
  items |> list.each { item -> print(item.display()) }
}
```

This says: "`print_all` works on any list, as long as the element type implements
`Display`."

### Built-in traits

Silt provides four built-in traits:

| Trait      | Purpose                              |
|------------|--------------------------------------|
| `Display`  | Convert to human-readable string     |
| `Compare`  | Order comparison                     |
| `Equal`    | Equality comparison                  |
| `Hash`     | Hash value for use in maps           |

All four traits are **automatically derived** for every user-defined type (both enum and
record types). The auto-derived `Display` formats values in constructor syntax:

```silt
type Color { Red, Green, Blue }
type Shape { Circle(Int), Rect(Int, Int) }

fn main() {
  println(Red.display())           -- "Red"
  println(Circle(5).display())     -- "Circle(5)"
  println(Rect(3, 4).display())    -- "Rect(3, 4)"
}
```

If the auto-derived format is not what you want, write your own `trait Display for T`
implementation -- it will override the auto-derived version:

```silt
trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "a circle of radius {r}"
      Rect(w, h) -> "a {w}x{h} rectangle"
    }
  }
}
```

### Full example

```silt
type Shape {
  Circle(Float)
  Rect(Float, Float)
}

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle(r={r})"
      Rect(w, h) -> "Rect({w}x{h})"
    }
  }
}

fn area(shape) {
  match shape {
    Circle(r) -> 3.14159 * r * r
    Rect(w, h) -> w * h
  }
}

fn main() {
  let shapes = [Circle(5.0), Rect(3.0, 4.0), Circle(1.0)]

  shapes
  |> list.map { s -> (s.display(), area(s)) }
  |> list.each { pair -> println("{pair}") }
}
```

---

## 11. String Interpolation

### Expressions in strings

Silt strings support inline expressions using curly braces:

```silt
let name = "world"
let greeting = "hello {name}"     -- "hello world"
```

Any expression can go inside the braces:

```silt
let n = 42
let msg = "the answer is {n}"                 -- "the answer is 42"
let debug = "result: {io.inspect(value)}"      -- calls io.inspect()
let math = "sum is {1 + 2 + 3}"              -- "sum is 6"
```

### Field access in interpolation

You can access record fields directly inside interpolation:

```silt
let user = User { name: "Alice", age: 30, active: true }
println("{user.name} is {user.age} years old")
```

### Escaping braces

If you need a literal `{` in a string, escape it with a backslash:

```silt
let json = "the format is \{\"key\": \"value\"}"
```

### Display trait and custom types

String interpolation automatically invokes the `Display` trait for all interpolated values.
Primitive types (`Int`, `Float`, `Bool`, `String`) and all user-defined types (enums and
records) implement `Display` automatically -- no manual implementation required:

```silt
type Color { Red, Green, Blue }
type Shape { Circle(Int), Rect(Int, Int) }

fn main() {
  let c = Red
  let s = Circle(5)
  println("color: {c}")    -- "color: Red"
  println("shape: {s}")    -- "shape: Circle(5)"
}
```

The auto-derived Display formats values in constructor syntax. To customize how a type
appears in interpolation, write your own Display implementation:

```silt
trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "a circle of radius {r}"
      Rect(w, h) -> "a {w}x{h} rectangle"
    }
  }
}

let s = Circle(5)
println("shape: {s}")    -- "shape: a circle of radius 5"
```

You do not need to call `.display()` explicitly inside interpolation -- it is called
automatically.

### Triple-quoted strings

For multiline text or content that contains quotes, backslashes, or braces, use
triple-quoted strings (`""" ... """`). They have three properties:

1. **No escape processing** -- `\n` is a literal backslash followed by `n`, not a newline
2. **No interpolation** -- `{expr}` is literal text, not an expression
3. **Indentation stripping** -- leading whitespace is stripped based on the closing `"""`

```silt
let json = """
  {
    "name": "Alice",
    "age": 30
  }
  """
-- Result: '{\n  "name": "Alice",\n  "age": 30\n}'
```

The indentation of the closing `"""` determines how much leading whitespace to strip
from each line. In the example above, the closing `"""` has 2 spaces of indent, so
2 spaces are stripped from each content line.

This is especially useful for embedding JSON, regex patterns, or any text that would
otherwise require heavy escaping:

```silt
let regex = """[\w]+@[\w]+\.\w{2,}"""  -- no need to escape backslashes or braces
let html = """
  <div class="greeting">
    <p>Hello, world!</p>
  </div>
  """
```

The first line after the opening `"""` is removed if blank, and the last line before
the closing `"""` is removed if blank. This means the opening `"""` and closing `"""`
can be on their own lines without adding extra blank lines to the result.

---

## 12. Modules & Visibility

### File = module

Every `.silt` file is a module. The file name determines the module name. There is no
module declaration syntax needed inside the file.

### Private by default

Everything in a module is private unless explicitly exported with `pub`:

```silt
-- file: math.silt

pub fn add(a, b) = a + b
pub fn subtract(a, b) = a - b

fn internal_helper(x) = x * 2        -- private, not visible outside

pub type Point {
  x: Float,
  y: Float,
}
```

Privacy by default matters because it gives you freedom to change internal
implementation details without breaking consumers. Only what you explicitly mark `pub`
becomes your API.

### Importing

There are three import forms:

```silt
-- Import the whole module (access via module.name)
import math

-- Import specific items directly into scope
import math.{ add, Point }

-- Import with an alias
import math as m
```

Using imported items:

```silt
import math
import math.{ add }

fn main() {
  let a = math.subtract(10, 3)    -- qualified access
  let b = add(1, 2)               -- direct access (imported by name)
}
```

### Globals and built-in modules

Silt's global namespace is deliberately small. Only 8 names are available without
any module qualification:

**Global names (always available, no import needed):**
`print`, `println`, `panic`, `try`, `Ok`, `Err`, `Some`, `None`

Everything else lives in a module and is accessed with dot notation. This keeps the
global namespace clean and makes it always clear where a function comes from.

**Standard library modules:**

| Module    | Provides                                                                        |
|-----------|---------------------------------------------------------------------------------|
| `io`      | inspect, read_file, write_file, read_line, args                                |
| `list`    | map, filter, fold, each, find, zip, flatten, flat_map, sort_by, any, all, ...  |
| `map`     | get, set, delete, keys, values, merge, length                                  |
| `string`  | split, join, trim, contains, replace, length, pad_left, pad_right, ...         |
| `int`     | parse, abs, min, max, to_float                                                 |
| `float`   | parse, round, ceil, floor, abs, min, max                                       |
| `result`  | map_ok, map_err, unwrap_or, flatten, is_ok, is_err                             |
| `option`  | map, unwrap_or, to_result, is_some, is_none                                    |
| `test`    | assert, assert_eq, assert_ne                                                   |
| `channel` | new, send, receive, close, select, try_send, try_receive                       |
| `task`    | spawn, join, cancel                                                            |

Module functions are accessed with dot notation:

```silt
fn main() {
  let parts = "hello world" |> string.split(" ")
  let upper = "hello" |> string.replace("hello", "HELLO")
  let n = "42" |> int.parse()
  let len = list.length([1, 2, 3])
}
```

---

## 13. Comments

### Line comments

Line comments start with `--` and run to the end of the line:

```silt
-- This is a comment
let x = 42  -- inline comment
```

### Block comments

Block comments are delimited by `{-` and `-}`. They can be nested, which makes it easy
to comment out code that already contains comments:

```silt
{- This is a block comment -}

{-
  This is a multi-line
  block comment
-}

{-
  Outer comment
  {- Inner comment -- nesting works -}
  Still in outer comment
-}
```

The `--` style was chosen for line comments because it is visually clean and avoids
conflicts with the `/` division operator. The `{- -}` style for block comments (borrowed
from Haskell) supports nesting, which `/* */` does not.

---

## Putting It All Together

Here is a complete program that demonstrates many of Silt's features working together:

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

  -- Pipeline: filter active users, age them up, print
  users
  |> list.filter { u -> u.active }
  |> list.map { u -> birthday(u) }
  |> list.each { u ->
    println("{u.name} is now {u.age}")
  }
}
```

And here is the classic FizzBuzz, showing pattern matching, tuples, pipes, trailing
closures, and string interpolation:

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
  1..101
  |> list.map { n -> fizzbuzz(n) }
  |> list.each { s -> println(s) }
}
```
