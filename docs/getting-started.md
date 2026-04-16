---
title: "Getting Started"
section: "Guide"
order: 1
description: "Install silt, write your first program, and tour the language through runnable examples."
---

# Getting Started

Silt is a statically-typed, expression-based language with a small, fixed keyword set, full immutability, and CSP-style concurrency. Pattern matching is the only way to branch. Types are inferred. Errors are values.

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
silt run
```

`silt init` creates a `silt.toml` manifest and a starter `src/main.silt`. `silt run` (with no arguments, inside the package directory) executes the package's entry point. That's the whole loop.

See `examples/` in the repository for runnable sample programs — start with `examples/hello.silt`, `examples/fizzbuzz.silt`, and `examples/records.silt`.

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
import io
import json

type Config { name: String }

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
import list

[1, 2, 3, 4, 5]
|> list.filter { n -> n > 2 }
|> list.map { n -> n * n }
|> list.fold(0) { acc, n -> acc + n }
```

## 7. Concurrency

Spawn lightweight tasks that run in parallel. Communicate through channels. I/O inside tasks transparently yields — no async/await.

```silt
import channel
import task

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
silt check --format json <file.silt>   -- type-check with JSON output (for CI/editors)
silt test [path]           -- run test functions
silt fmt [files...]        -- format source code
silt fmt --check           -- check formatting without modifying files
silt repl                  -- interactive REPL
silt init                  -- create a new silt package in the current directory
silt lsp                   -- start the language server
silt disasm <file.silt>    -- show bytecode disassembly (same as `silt run --disassemble`)
silt self-update           -- update the silt binary to the latest release
silt update                -- update package dependencies (coming in v0.7)
silt add <name> --path <path>  -- add a path-based dependency to silt.toml
silt add <name> --git <url> [--rev|--branch|--tag <ref>]  -- add a git-based dependency to silt.toml
```

The `--watch` / `-w` flag works with `run`, `check`, and `test` to automatically re-run on `.silt` file changes.

### Staying up to date

Run `silt self-update` to replace the installed binary with the latest GitHub release. It detects your platform, fetches the prebuilt archive, verifies it against the release's SHA-256 checksum, and atomically swaps the binary in place — no need to re-run the install script. Verification is fail-closed: a mismatch or missing `SHA256SUMS` file aborts the update without touching the installed binary. Pass `--dry-run` to preview the version that would be installed, or `--force` to reinstall when already current.

(In v0.7 the bare `silt update` is being repurposed to update package dependencies; the binary self-update is now spelled `silt self-update`.)

## What's next

- **[Language Guide](language-guide.md)** — complete coverage of every feature
- **[Standard Library](stdlib-reference.md)** — all modules and functions
- **[Concurrency](concurrency.md)** — the full CSP model, channels, and select
- **[FFI Guide](ffi.md)** — embed silt in Rust applications
- **[Editor Setup](editor-setup.md)** — configure your editor for silt
