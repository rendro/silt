# silt

A delightful language for programmers who've suffered enough. Types without annotations. Threads without locks. Errors without surprises.

```silt
import list

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
type Shape { Circle(Int), Square(Int), Triangle(Int, Int) }

fn area(s: Shape) -> Int {
  match s {
    Circle(r) -> 3 * r * r
    Square(w) -> w * w
    Triangle(b, h) -> b * h / 2
  }
}
```

## Parallelism

Spawn lightweight tasks that run in parallel on a fixed thread pool. Communicate through channels. Every value is immutable, so there are no data races to debug. I/O operations transparently yield to the scheduler — no async/await needed.

```silt
import channel
import list
import task

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
import io
import json

type Config { name: String }

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

The installer verifies the downloaded binary against the release's `SHA256SUMS` file before extracting — a mismatch aborts the install so a corrupted or tampered archive can't land on disk. To upgrade an existing install without re-running the curl pipe, use `silt self-update` (which performs the same verification).

Or build from source:

```sh
git clone https://github.com/rendro/silt.git
cd silt && cargo build --release
cp target/release/silt ~/.local/bin/
```

Then:

```sh
silt init
silt run
```

`silt init` writes a `silt.toml` manifest and a starter `src/main.silt`; `silt run` (no arguments, inside a package) executes the entry point.

See `examples/` for runnable sample programs — start with `examples/hello.silt`, `examples/fizzbuzz.silt`, and `examples/records.silt`.

## Tooling

```
silt run <file.silt>       Run a program
silt run -w <file.silt>    Run and re-run on file changes
silt check <file.silt>     Type-check without running
silt check --format json <file.silt>   Type-check with JSON output (for CI/editors)
silt test [path]           Run test functions
silt fmt [files...]        Format source code
silt fmt --check           Check formatting without modifying files
silt repl                  Interactive REPL
silt init                  Create a new silt package in the current directory
silt lsp                   Start the language server
silt disasm <file.silt>    Show bytecode disassembly (same as `silt run --disassemble`)
silt self-update           Update the silt binary to the latest release
```

The `--watch` / `-w` flag works with `run`, `check`, and `test`. It watches the project directory for `.silt` file changes and automatically re-runs the command.

LSP server with diagnostics, hover types, go-to-definition, completion, signature help, document symbols, and formatting. The prebuilt `silt` binary from the install script includes the LSP server — just run `silt lsp` and point your editor at it. Vim/Neovim syntax highlighting and editor setup ship in `editors/`.

## Reference

| Feature | Details |
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
