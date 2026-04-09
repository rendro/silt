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

Spawn lightweight tasks that run in parallel on a fixed thread pool. Communicate through channels. Every value is immutable, so there are no data races to debug. I/O operations transparently yield to the scheduler — no async/await needed.

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

## HTTP

Built-in HTTP client and server. Pattern matching replaces routing frameworks. Requests are handled concurrently.

```silt
import http
import json

type Todo { id: Int, title: String, done: Bool }

fn main() {
  http.serve(8080, fn(req) {
    match (req.method, http.segments(req.path)) {
      (GET, ["todos"]) -> {
        let todos = [
          Todo { id: 1, title: "Learn silt", done: true },
          Todo { id: 2, title: "Build an API", done: false },
        ]
        Response { status: 200, body: json.stringify(todos), headers: #{} }
      }
      (POST, ["todos"]) ->
        match json.parse(Todo, req.body) {
          Ok(todo) -> Response { status: 201, body: json.stringify(todo), headers: #{} }
          Err(e) -> Response { status: 400, body: e, headers: #{} }
        }
      _ ->
        Response { status: 404, body: "Not found", headers: #{} }
    }
  })
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
curl -fsSL https://silt-lang.com/install.sh | sh
```

Or build from source:

```sh
git clone https://github.com/rendro/silt.git
cd silt && cargo build --release
cp target/release/silt ~/.local/bin/
```

Then:

```sh
silt init
silt run main.silt
```

## Tooling

```
silt run <file.silt>       Run a program
silt run -w <file.silt>    Run and re-run on file changes
silt check <file.silt>     Type-check without running
silt test [path]           Run test functions
silt fmt <file.silt>       Format source code
silt repl                  Interactive REPL
silt init                  Create a new main.silt
silt lsp                   Start the language server
```

The `--watch` / `-w` flag works with `run`, `check`, and `test`. It watches the project directory for `.silt` file changes and automatically re-runs the command.

LSP server with diagnostics, hover types, go-to-definition, completion, signature help, document symbols, and formatting. Vim/Neovim syntax highlighting ships in `editors/vim/`.

## Reference

|---|---|
| **keywords** | `as else fn import let loop match mod pub return trait type when where` |
| **types** | inferred, with ADTs, records, and traits |
| **literals** | `42`, `0xFF`, `0b1010`, `1e5`, `1_000`, `"hi {x}"` |
| **branching** | match only |
| **mutability** | none |
| **errors** | Result / Option / ? |
| **concurrency** | CSP with real parallelism |
| **collections** | `[1, 2, 3]` list, `#{"k": "v"}` map, `#[1, 2]` set |
| **stdlib** | small but exhaustive |
| **tools** | REPL, formatter, test runner, LSP |

## Documentation

Full documentation and an interactive playground are at [silt-lang.com](https://silt-lang.com).

## License

MIT
