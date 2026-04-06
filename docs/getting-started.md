---
title: "Getting Started"
order: 1
description: "Install silt, write your first program, and tour the language through runnable examples."
---

# Getting Started

Silt is a statically-typed, expression-based language with 14 keywords, full immutability, and CSP-style concurrency. Pattern matching is the only way to branch. Types are inferred. Errors are values.

This guide walks you through the essentials. For the complete reference, see the [Language Guide](language-guide.md).

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

## Your first program

```sh
silt init
silt run main.silt
```

`silt init` creates a starter `main.silt`. `silt run` executes it. That's the whole loop.

## 1. Bindings

Everything is immutable. `let` binds a name to a value. You can shadow, but you can't reassign.

```silt
let x = 42
let x = x + 1   -- shadows, x is now 43
```

## 2. Functions

```silt
fn add(a, b) {
  a + b
}
```

The last expression is the return value. No `return` keyword needed (though it exists for early exits).

Anonymous functions:

```silt
let double = fn(x) { x * 2 }
```

## 3. Pattern matching

The only branching construct. Match on constructors, tuples, lists, records, guards, ranges, or-patterns.

```silt
fn describe(n) {
  match n {
    0 -> "zero"
    1 | 2 | 3 -> "small"
    _ when n < 0 -> "negative"
    _ -> "big"
  }
}
```

Match destructures:

```silt
let (x, y) = (1, 2)

match items {
  [] -> "empty"
  [head, ..tail] -> "non-empty"
}
```

## 4. Types

Types are inferred. You only write them for declarations:

```silt
type User {
  name: String,
  age: Int,
}

type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn area(shape) {
  match shape {
    Circle(r) -> 3.14 * r * r
    Rect(w, h) -> w * h
  }
}
```

## 5. Errors as values

No exceptions. Fallible functions return `Result`. The `?` operator propagates errors:

```silt
fn read_config(path) {
  let content = io.read_file(path)?
  let config = json.parse(Config, content)?
  Ok(config)
}
```

Use `match` to handle the result:

```silt
match read_config("app.json") {
  Ok(cfg) -> println("loaded: {cfg.name}")
  Err(e) -> println("error: {e}")
}
```

## 6. Pipes and trailing closures

The `|>` operator passes the left value as the first argument of the right:

```silt
[1, 2, 3, 4, 5]
|> list.filter { n -> n > 2 }
|> list.map { n -> n * n }
|> list.fold(0) { acc, n -> acc + n }
```

## 7. Concurrency

Spawn tasks that run in parallel. Communicate through channels.

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

## Tooling

```sh
silt run <file.silt>       -- run a program
silt run -w <file.silt>    -- run and re-run on file changes
silt check <file.silt>     -- type-check without running
silt test [path]           -- run test functions
silt fmt <file.silt>       -- format source code
silt repl                  -- interactive REPL
silt lsp                   -- start the language server
```

The `--watch` / `-w` flag works with `run`, `check`, and `test` to automatically re-run on `.silt` file changes.

## What's next

- **[Language Guide](language-guide.md)** — complete coverage of every feature
- **[Standard Library](stdlib-reference.md)** — all modules and functions
- **[Concurrency](concurrency.md)** — the full CSP model, channels, and select
- **[FFI Guide](ffi.md)** — embed silt in Rust applications
- **[Editor Setup](editor-setup.md)** — configure your editor for silt
