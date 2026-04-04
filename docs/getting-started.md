# Getting Started with Silt

## What is Silt?

Silt is a statically-typed, expression-based language with 14 keywords, full immutability, and CSP-style concurrency. Pattern matching is the sole branching construct -- there is no `if`/`else`, no `for`/`while`, no `mut`, and no exceptions. Types are inferred via Hindley-Milner, errors are values (`Result`/`Option`), and concurrent tasks communicate through typed channels.

## Installation

Silt is implemented in Rust. Install it from a local checkout:

```sh
cargo install --path .
```

Or run programs directly without installing:

```sh
cargo run -- run file.silt
```

Verify the installation:

```sh
silt run examples/hello.silt
```

## Hello World

```silt
fn main() {
  println("hello, world")
}
```

Save this as `hello.silt` and run it with `silt run hello.silt`. Every silt program starts at `fn main()`.

---

## Language Tour

### Bindings

All bindings are immutable. There is no reassignment -- only shadowing.

```silt
fn main() {
  let x = 42
  let name = "Alice"
  let x = x + 1    -- shadowing, not mutation; x is now 43
  println("{name} has {x}")
}
```

Type annotations are optional thanks to Hindley-Milner inference:
`let x: Int = 42`, `let ratio: Float = 3.14`.

### Functions

Named functions use `fn`. The last expression in the body is the return value.

```silt
fn add(a, b) {
  a + b
}

-- Single-expression shorthand with =
fn square(x) = x * x

fn main() {
  println("{add(2, 3)}")   -- 5
  println("{square(4)}")   -- 16
}
```

Functions are values. Anonymous functions close over their environment:

```silt
fn main() {
  let double = fn(x) { x * 2 }
  println("{double(5)}")   -- 10
}
```

### Pattern Matching

`match` is the only branching construct. No `if`/`else` exists in Silt.

**With a scrutinee** -- match a value against patterns:

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
  1..21 |> list.each { n -> println(fizzbuzz(n)) }
}
```

**Without a scrutinee** -- acts like a conditional chain:

```silt
fn classify(n) {
  match {
    n == 0 -> "zero"
    n > 0  -> "positive"
    _      -> "negative"
  }
}

fn main() {
  println(classify(5))    -- positive
  println(classify(-3))   -- negative
  println(classify(0))    -- zero
}
```

**Guards** -- add `when` conditions to arms:

```silt
fn describe(n) {
  match n {
    0 -> "zero"
    x when x > 100 -> "big: {x}"
    x when x > 0   -> "positive: {x}"
    _               -> "negative"
  }
}

fn main() {
  println(describe(150))   -- big: 150
  println(describe(-1))    -- negative
}
```

**Or-patterns** -- match multiple alternatives in one arm:

```silt
type Color { Red, Green, Blue, Yellow, Cyan }

fn is_primary(c) {
  match c {
    Red | Blue | Yellow -> true
    _ -> false
  }
}

fn main() {
  println("{is_primary(Red)}")    -- true
  println("{is_primary(Cyan)}")   -- false
}
```

**`when`/`else` guards** -- flat early-return style. The `else` block must
diverge (via `return` or `panic`):

```silt
fn parse_positive(s) {
  when Ok(n) = int.parse(s) else {
    return Err("not a number")
  }
  when n > 0 else {
    return Err("not positive")
  }
  Ok(n)
}

fn main() {
  match parse_positive("42") {
    Ok(n)  -> println("got {n}")
    Err(e) -> println("error: {e}")
  }
}
```

### Types

**Algebraic data types (ADTs)** -- define enums with `type`:

```silt
type Shape {
  Circle(Float)
  Rect(Float, Float)
}

fn area(shape) {
  match shape {
    Circle(r) -> 3.14159 * r * r
    Rect(w, h) -> w * h
  }
}

fn main() {
  let shapes = [Circle(5.0), Rect(3.0, 4.0)]
  shapes |> list.each { s -> println("{area(s)}") }
}
```

Negative patterns work in matches: `Num(-1)`, `0.0..1.0`.

**Records** -- named fields with `type`:

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
  let u = User { name: "Alice", age: 30, active: true }
  let u = birthday(u)
  println("{u.name} is {u.age}")   -- Alice is 31
}
```

Record update syntax (`user.{ field: value }`) returns a new value -- nothing
is mutated.

**Tuples** -- lightweight grouping with parentheses:

```silt
fn swap(pair) {
  let (a, b) = pair
  (b, a)
}

fn main() {
  let (x, y) = swap((1, 2))
  println("{x}, {y}")   -- 2, 1
}
```

### Traits

Traits define shared behavior. All user-defined types auto-derive `Display`,
`Compare`, `Equal`, and `Hash`.

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

fn main() {
  let s = Circle(5.0)
  println(s.display())   -- Circle(r=5)
}
```

**`where` clauses** constrain generic type parameters. You must use explicit
type annotations -- `fn f(x) where a: Display` is an error:

```silt
fn print_all(items: List(a)) where a: Display {
  items |> list.each { item -> println(item.display()) }
}

fn main() {
  print_all([Circle(1.0), Rect(2.0, 3.0)])
}
```

### Pipe Operator

`|>` passes the left-hand value as the **first argument** to the right-hand
function. Trailing closures go after the call:

```silt
fn main() {
  let result =
    [1, 2, 3, 4, 5]
    |> list.filter { x -> x > 2 }
    |> list.map { x -> x * 10 }
    |> list.fold(0) { acc, x -> acc + x }

  println("{result}")   -- 120
}
```

Without pipes, the same code nests inward and reads inside-out.

### String Interpolation

Expressions inside `{...}` are evaluated and interpolated:

```silt
fn main() {
  let name = "world"
  let n = 6
  println("hello {name}, 2+2={2 + 2}, n*7={n * 7}")
}
```

Triple-quoted strings (`"""..."""`) disable interpolation and escape processing.
Use them for regex patterns and literal braces:

```silt
fn main() {
  let pattern = """\d{2}:\d{2}"""
  println(pattern)   -- \d{2}:\d{2}
}
```

### Error Handling

Errors are values, not exceptions. Two types: `Result(a, e)` with `Ok`/`Err`,
and `Option(a)` with `Some`/`None`.

**`?` operator** -- propagates errors early:

```silt
fn add_strings(a, b) {
  let x = int.parse(a)?
  let y = int.parse(b)?
  Ok(x + y)
}

fn main() {
  match add_strings("10", "20") {
    Ok(n)  -> println("{n}")
    Err(e) -> println("error: {e}")
  }
}
```

**`when`/`else`** provides flat error handling without nesting (see Pattern
Matching section above).

`return` and `panic()` produce the `Never` type, which unifies with any type
in match arms.

### Collections

**Lists** -- ordered, variable-length, `[]` syntax. List patterns destructure
by shape:

```silt
fn sum(xs) {
  match xs {
    [] -> 0
    [head, ..tail] -> head + sum(tail)
  }
}

fn main() {
  println("{sum([1, 2, 3, 4, 5])}")   -- 15
}
```

**Maps** -- key-value pairs with `#{}` syntax:

```silt
fn main() {
  let m = #{ "name": "Alice", "role": "admin" }
  match map.get(m, "name") {
    Some(name) -> println("found: {name}")
    None -> println("not found")
  }
  println("{map.contains(m, "role")}")   -- true
}
```

Use `map.contains` to check key membership.

**Sets** -- unique values with `#[]` syntax:

```silt
fn main() {
  let a = #[1, 2, 3]
  let b = #[3, 4, 5]
  let both = set.intersection(a, b)
  println("{set.to_list(both)}")   -- [3]
  println("{set.contains(a, 2)}")  -- true
}
```

### Loop Expressions

`loop` binds initial state and re-enters with `loop(new_values)`. Any
non-`loop()` expression terminates and becomes the result. There is no
`for`/`while` -- use `loop` for general iteration and `list.map`/`list.each`
for collection traversal.

```silt
fn main() {
  -- Sum 0 through 9
  let total = loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      _ -> loop(i + 1, acc + i)
    }
  }
  println("sum: {total}")   -- sum: 45
}
```

Zero-binding form for infinite-style loops (use `loop()` to re-enter, or
produce any other value to terminate):

```silt
fn main() {
  let ch = channel.new(5)
  channel.send(ch, "one")
  channel.send(ch, "two")
  channel.close(ch)

  loop {
    match channel.receive(ch) {
      Message(msg) -> {
        println(msg)
        loop()
      }
      Closed -> println("done")
    }
  }
}
```

### Concurrency

Silt uses CSP (Communicating Sequential Processes): tasks communicate through
channels, not shared memory. All concurrency primitives are module-qualified.

```silt
fn main() {
  let ch = channel.new(10)

  -- Spawn a producer task
  let producer = task.spawn(fn() {
    channel.send(ch, "hello")
    channel.send(ch, "from")
    channel.send(ch, "silt")
    channel.close(ch)
  })

  -- Spawn a consumer task
  let consumer = task.spawn(fn() {
    let Message(a) = channel.receive(ch)
    let Message(b) = channel.receive(ch)
    let Message(c) = channel.receive(ch)
    println("{a} {b} {c}")
  })

  task.join(producer)
  task.join(consumer)
}
```

**`channel.select`** waits on multiple channels. Use `^pin` to identify which
channel fired:

```silt
fn main() {
  let urgent = channel.new(5)
  let normal = channel.new(5)

  channel.send(urgent, "alert!")
  channel.send(normal, "background done")

  match channel.select([urgent, normal]) {
    (^urgent, Message(msg)) -> println("URGENT: {msg}")
    (^normal, Message(msg)) -> println("Normal: {msg}")
    (_, Closed) -> println("channel closed")
  }
}
```

---

## Running Programs

```sh
silt run <file.silt>       -- run a program
silt test [file.silt]      -- run test functions
silt repl                  -- interactive read-eval-print loop
silt fmt <file.silt>       -- format a source file
```

During development, you can also run with cargo:

```sh
cargo run -- run file.silt
```

---

## What's Next

- **[Language Guide](language-guide.md)** -- complete coverage of every feature
- **[Standard Library Reference](stdlib-reference.md)** -- all modules and functions
- **[Concurrency Guide](concurrency.md)** -- channels, tasks, select, and scheduling
