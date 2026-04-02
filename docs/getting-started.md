# Getting Started with Silt

## What is Silt?

Silt is a minimal, statically-typed, expression-based programming language with CSP-style concurrency. It compiles to a tree-walk interpreter written in Rust.

The language is built around a small set of principles: just 14 keywords, fully immutable bindings, pattern matching as the sole branching construct, and explicit error handling through `Result` and `Option` -- no exceptions, no null. Types are inferred via Hindley-Milner, so you rarely need to write annotations. Concurrency is modeled after Communicating Sequential Processes with typed channels, tasks, and `channel.select`.

The global namespace is deliberately small: only 13 names (`print`, `println`, `panic`, `try`, `Ok`, `Err`, `Some`, `None`, `Stop`, `Continue`, `Message`, `Closed`, `Empty`) are available without qualification. Everything else lives in a module (`list.map`, `string.split`, `channel.new`, `task.spawn`, etc.).

If you like the safety of Rust, the expressiveness of ML-family languages, and the simplicity of Go's concurrency model -- but want something you can learn in an afternoon -- Silt might be for you.

## Installation

Silt is implemented in Rust. To build from source:

```sh
git clone https://github.com/rendro/silt.git
cd silt
cargo build --release
```

The compiled binary is at `target/release/silt`. Add it to your PATH:

```sh
cp target/release/silt ~/.local/bin/
```

Verify it works:

```sh
silt run examples/hello.silt
```

## CLI Commands

```sh
silt run <file.silt>       -- run a program
silt test [file.silt]      -- run test functions
silt repl                  -- interactive read-eval-print loop
silt fmt <file.silt>       -- format a source file
```

## Your First Program

Create a file called `hello.silt`:

```silt
fn main() {
  println("hello, world")
}
```

Run it:

```sh
silt run hello.silt
```

Every Silt program needs a `main()` function as its entry point. The `println` function is one of 13 global builtins -- always available, no import needed.

## Language Tour

### Bindings

All bindings are immutable. There is no mutable state. You can shadow a binding by re-declaring it with `let`:

```silt
let x = 42
let name = "Alice"
let x = x + 1   -- shadowing, not mutation
```

Type annotations are optional -- the compiler infers types for you:

```silt
let x: Int = 42   -- valid, but rarely needed
```

Bindings can also appear at the top level of a file, outside any function:

```silt
let default_port = 8080

fn main() {
  println("port: {default_port}")
}
```

### Functions

Functions are expressions. The last expression in the body is the return value:

```silt
fn add(a, b) {
  a + b
}
```

For single-expression functions, use the shorthand form:

```silt
fn square(x) = x * x
```

Anonymous functions work too:

```silt
let double = fn(x) { x * 2 }
```

### Pattern Matching

`match` is the only branching construct in Silt. There are no `if`/`else` statements. The compiler checks that your matches are exhaustive:

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

Guards let you add conditions to match arms:

```silt
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}
```

Guardless match lets you branch on boolean conditions without a scrutinee:

```silt
fn classify(n) {
  match {
    n == 0 -> "zero"
    n > 0  -> "positive"
    _      -> "negative"
  }
}
```

The `^` pin operator matches against an existing variable's value instead of binding:

```silt
let expected = 42
match input {
  ^expected -> "got what we wanted"
  other -> "got {other} instead"
}
```

### The Pipe Operator

The `|>` operator passes the left-hand side as the first argument to the right-hand side. This makes data transformation pipelines easy to read:

```silt
1..101
|> list.map { n -> fizzbuzz(n) }
|> list.each { s -> println(s) }
```

### Trailing Closures

When the last argument to a function is a closure, you can write it outside the parentheses using `{ args -> body }` syntax:

```silt
-- these are equivalent
[1, 2, 3] |> list.map(fn(x) { x * 2 })
[1, 2, 3] |> list.map { x -> x * 2 }

-- multi-line
users |> list.filter { user ->
  user.age > 18 && user.active
}

-- multiple args
[1, 2, 3] |> list.fold(0) { acc, x -> acc + x }
```

### String Interpolation

Curly braces inside strings evaluate expressions. The expression must implement the `Display` trait:

```silt
let name = "Alice"
let age = 30
println("hello {name}, you are {age} years old")
println("2 + 2 = {2 + 2}")
```

Use `\{` to include a literal brace.

### Error Handling

Silt has no exceptions. All errors are represented as values using `Result` and `Option`:

```silt
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  let host = host_line |> string.replace("host=", "")
  Ok(host)
}
```

The `?` operator provides sugar for early returns on `Err` or `None`:

```silt
fn process(input) {
  let n = parse_int(input)?       -- returns Err early if parse fails
  let result = validate(n)?       -- same here
  Ok(result * 2)
}
```

The `try()` builtin wraps a function call in a `Result`, catching any runtime errors:

```silt
let result = try(fn() { risky_operation() })
-- Ok(value) on success, Err(message) on failure
```

Use `match` for explicit handling, `?` for propagation, and `when`-`else` for inline assertions with destructuring:

```silt
-- explicit match
match parse_int("42") {
  Ok(n) -> println("got {n}")
  Err(e) -> println("failed: {e}")
}

-- pipe-friendly combinators
parse_int("42")
|> result.map_ok { n -> n * 2 }
|> result.unwrap_or(0)
```

### Types

Silt has algebraic data types (tagged unions) and records.

**Algebraic types** define a set of variants, each optionally carrying data:

```silt
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn area(shape) {
  match shape {
    Circle(r) -> 3.14159 * r * r
    Rect(w, h) -> w * h
  }
}
```

**Records** are types with named fields:

```silt
type User {
  name: String,
  age: Int,
  active: Bool,
}

let u = User { name: "Alice", age: 30, active: true }
println(u.name)                  -- "Alice"
let u2 = u.{ age: 31 }          -- record update (returns new record)
```

### Loop Expressions

`loop` provides stack-safe iteration. It binds initial state, evaluates a body, and re-enters via `loop(new_values)`. Any expression that is not `loop(...)` terminates the loop and becomes its value.

```silt
-- Sum 0..9
let total = loop i = 0, acc = 0 {
  match i >= 10 {
    true -> acc
    _ -> loop(i + 1, acc + i)
  }
}
println(total)  -- 45
```

Zero-binding loops work for infinite-style iteration:

```silt
loop {
  match io.read_line() {
    Ok("quit") -> println("goodbye")
    Ok(line) -> { println("echo: {line}"); loop() }
    _ -> println("goodbye")
  }
}
```

### Traits

Traits define interfaces that types can implement. Silt has four built-in traits: `Display`, `Compare`, `Equal`, and `Hash`.

```silt
type Shape {
  Circle(Float),
  Rect(Float, Float),
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
  println(s.display())          -- "Circle(r=5)"
  println("shape: {s.display()}")  -- string interpolation calls display
}
```

Traits can be used as constraints with `where`:

```silt
fn print_all(items) where a: Display {
  items |> list.each { item -> println(item.display()) }
}
```

### Unary Operators

Silt supports unary negation (`-`) for numbers and logical not (`!`) for booleans:

```silt
let x = 42
println(-x)      -- -42
println(-3.14)   -- -3.14
println(!true)   -- false
```

## Working with Files

### Running a program

```sh
silt run myfile.silt
```

This finds the `main()` function in the file and executes it. You can also omit the `run` subcommand for `.silt` files:

```sh
silt myfile.silt
```

### Interactive REPL

```sh
silt repl
```

Launches an interactive session where you can evaluate expressions and define bindings on the fly.

### Formatting code

```sh
silt fmt myfile.silt
```

Formats a Silt source file according to the standard style.

### Running tests

Test functions are any function whose name starts with `test_`. Put them in files ending with `_test.silt`:

```silt
-- math_test.silt
import math

fn test_add() {
  test.assert_eq(math.add(1, 2), 3)
  test.assert_eq(math.add(-1, 1), 0)
}

fn test_square() {
  test.assert_eq(math.square(5), 25)
}
```

Run a specific test file:

```sh
silt test math_test.silt
```

Or discover and run all test files in the current directory:

```sh
silt test
```

This finds all `*_test.silt` and `*.test.silt` files automatically.

## Project Structure

In Silt, each file is a module. The filename (without `.silt`) is the module name. Everything in a file is private by default -- use `pub` to export:

```silt
-- math.silt
pub fn add(a, b) = a + b
pub fn square(x) = x * x
fn internal_helper(x) = x * 2   -- private, not visible to importers
```

Other files import modules by name:

```silt
-- main.silt
import math

fn main() {
  let result = math.add(3, 4)
  println("3 + 4 = {result}")
  println("5^2 = {math.square(5)}")
}
```

You can also import specific items or rename modules:

```silt
import math.{ add, square }    -- import specific functions
import math as m                -- rename the module
```

### Example project layout

```
my-project/
  main.silt          -- entry point with main()
  math.silt          -- pub functions for math operations
  user.silt          -- pub type User and related functions
  math_test.silt     -- tests for math module
  user_test.silt     -- tests for user module
```

Run the project with `silt run main.silt` from the project directory. The interpreter resolves imports relative to the entry file's directory.

## Where to Go Next

- **Language Guide** -- `docs/language-guide.md` -- deep dive into all language features
- **Standard Library Reference** -- `docs/stdlib-reference.md` -- every module, function, and type
- **Concurrency Guide** -- `docs/concurrency.md` -- channels, tasks, `channel.select`, and patterns
- **Design Decisions** -- `docs/design-decisions.md` -- why Silt is the way it is
- **Examples** -- the `examples/` directory has working programs covering records, traits, error handling, concurrency, and multi-file projects
