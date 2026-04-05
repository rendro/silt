# silt

A delightful language for programmers who've suffered enough. Types without annotations. Threads without locks. Errors without surprises.

```silt
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _ -> "{n}"
  }
}

fn main() {
  1..21
  |> list.each { n -> println(fizzbuzz(n)) }
}
```

## Pattern matching

The only way to branch. Define your types, then match on their shape. The compiler verifies every case is handled.

```silt
type Expr {
  Num(Int),
  Add(Expr, Expr),
  Mul(Expr, Expr)
}

fn eval(expr) {
  match expr {
    Num(n) -> n
    Add(a, b) -> eval(a) + eval(b)
    Mul(a, b) -> eval(a) * eval(b)
  }
}
```

## Parallelism

Spawn tasks that run in parallel. Communicate through channels. Every value is immutable, so there are no data races to debug.

```silt
fn main() {
  let ch = channel.new(10)

  let w1 = task.spawn(fn() {
    channel.each(ch) { msg -> println("w1: {msg}") }
  })
  let w2 = task.spawn(fn() {
    channel.each(ch) { msg -> println("w2: {msg}") }
  })

  list.each(1..100) { n -> channel.send(ch, n) }
  channel.close(ch)

  task.join(w1)
  task.join(w2)
}
```

## Errors as values

Every function that can fail returns a Result. The `?` operator propagates errors without nesting. Nothing is thrown or caught.

```silt
fn read_config(path) {
  let content = io.read_file(path)?
  let config = json.parse(Config, content)?
  Ok(config)
}

fn main() {
  match read_config("settings.json") {
    Ok(cfg) -> println("loaded: {cfg.name}")
    Err(e) -> println("error: {e}")
  }
}
```

## Type inference

The type checker infers everything. You get static type safety without writing annotations. Define records, enums, and traits when you need structure.

```silt
type User {
  name: String,
  age: Int,
}

fn greet(user) {
  "hello, {user.name} ({user.age})"
}

fn main() {
  let u = User { name: "alice", age: 30 }
  println(greet(u))
  println(greet(u.{ age: 31 }))  -- record update
}
```

## Install

```sh
git clone https://github.com/rendro/silt.git
cd silt && cargo build --release
cp target/release/silt ~/.local/bin/

silt init
silt run main.silt
```

## Tooling

```
silt run <file.silt>       Run a program
silt check <file.silt>     Type-check without running
silt test [path]           Run test functions
silt fmt <file.silt>       Format source code
silt repl                  Interactive REPL
silt init                  Create a new main.silt
silt lsp                   Start the language server
```

LSP server with diagnostics, hover types, go-to-definition, completion, signature help, document symbols, and formatting. Vim/Neovim syntax highlighting ships in `editors/vim/`.

## Reference

| | |
|---|---|
| **keywords** | `as else fn import let loop match mod pub return trait type when where` |
| **types** | inferred, with ADTs, records, and traits |
| **branching** | match only |
| **mutability** | none |
| **errors** | Result / Option / ? |
| **concurrency** | CSP with real parallelism |
| **collections** | `[1, 2, 3]` list, `#{"k": "v"}` map, `#[1, 2]` set |
| **stdlib** | small but exhaustive |
| **tools** | REPL, formatter, test runner, LSP |

## Documentation

Full documentation and an interactive playground are at [siltlang.dev](https://siltlang.dev).

## License

MIT
