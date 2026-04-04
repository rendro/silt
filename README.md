# silt

A statically-typed, expression-based language with 14 keywords, full immutability, and real parallelism.

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
  1..21 |> list.each { n -> println(fizzbuzz(n)) }
}
```

## Features

**Minimal by design.** 14 keywords. Pattern matching is the only branching construct. No `if`, no `while`, no `for`. Types are inferred via Hindley-Milner — you rarely write them.

**Fully immutable.** No `mut`, no reassignment. Every value is immutable. Collections return new copies. This makes concurrency safe by default.

**Real parallelism.** `task.spawn` runs on OS threads. Channels synchronize across threads. No locks, no data races, no async/await coloring.

```silt
fn main() {
  let ch = channel.new(10)

  let worker = task.spawn(fn() {
    channel.each(ch) { msg ->
      println("got: {msg}")
    }
  })

  channel.send(ch, "hello")
  channel.send(ch, "world")
  channel.close(ch)
  task.join(worker)
}
```

**Pattern matching everywhere.** Constructors, tuples, lists, records, maps, ranges, or-patterns, guards, pin patterns.

```silt
type Expr {
  Num(Int),
  Add(Expr, Expr),
  Mul(Expr, Expr),
}

fn eval(expr) {
  match expr {
    Num(n) -> n
    Add(a, b) -> eval(a) + eval(b)
    Mul(a, b) -> eval(a) * eval(b)
  }
}
```

**Errors are values.** `Result` and `Option` types with `?` for propagation. No exceptions.

```silt
fn read_config(path) {
  let content = io.read_file(path)?
  let config = json.parse(Config, content)?
  Ok(config)
}
```

**160+ stdlib functions** across 17 modules: list, string, int, float, map, set, result, option, io, math, channel, task, regex, json, test, fs.

## Getting Started

```sh
# Build from source
git clone https://github.com/your-user/silt.git
cd silt && cargo build --release
cp target/release/silt ~/.local/bin/

# Create a new project
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

**LSP server** with diagnostics, hover types, go-to-definition, completion, signature help, document symbols, and formatting. Works with any editor — Neovim, VS Code, etc.

**Vim/Neovim syntax highlighting** ships in `editors/vim/`.

## Embedding in Rust

Silt can be embedded as a scripting language. Register Rust functions callable from silt:

```rust
use silt::{Vm, Value};

let mut vm = Vm::new();
vm.register_fn1("double", |x: i64| -> i64 { x * 2 });
// silt code can now call: double(21)  -- returns 42
```

See [docs/ffi.md](docs/ffi.md) for the full FFI guide.

## Documentation

- [Getting Started](docs/getting-started.md) — installation, hello world, language tour
- [Language Guide](docs/language-guide.md) — complete reference for every feature
- [Standard Library](docs/stdlib-reference.md) — 160+ functions across 17 modules
- [Concurrency](docs/concurrency.md) — CSP model, channels, tasks, real parallelism
- [FFI Guide](docs/ffi.md) — embed silt in Rust, register foreign functions
- [Editor Setup](docs/editor-setup.md) — LSP, Neovim, VS Code, syntax highlighting

## Design

| | |
|---|---|
| **Keywords** | `as else fn import let loop match mod pub return trait type when where` |
| **Types** | Hindley-Milner inference, ADTs, records, traits |
| **Branching** | `match` only — with guards, or-patterns, ranges, destructuring |
| **Mutability** | None. Shadowing only. |
| **Errors** | `Result`/`Option` values, `?` operator |
| **Concurrency** | CSP: OS threads, typed channels, `channel.select` |
| **Collections** | `[]` list, `#{}` map, `#[]` set — all immutable |
| **FFI** | Register Rust functions with auto-marshalling |

## License

MIT
