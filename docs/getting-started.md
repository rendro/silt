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

Everything is immutable. `let` binds a name to a value. You can shadow within a block or function body, but you can't reassign. (Top-level names must be unique at module scope, so shadowing applies to local bindings.)

```silt
fn main() {
  let x = 42
  let x = x + 1 -- shadows, x is now 43
  print(x)
}
```

## 2. Functions

```silt
fn add(a, b) {
  a + b
}
```

The last expression is the return value. No `return` keyword needed (though it exists for early exits).

Closures are written in braces, parameters before the `->`:

```silt
let double = { x -> x * 2 }
let add = { x: Int, y: Int -> x + y }
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

## 5. Modules and imports

Every `.silt` file is a module. Items are private unless marked `pub`. Import a module to use its contents:

```silt
import geometry -- qualified:  geometry.area(3, 4)
import geometry as g -- aliased:    g.area(3, 4)
import geometry.{ area, perim } -- direct:     area(3, 4)
```

`import` brings in both stdlib modules (`list`, `io`, `channel`, …) and your own files. If `src/geometry.silt` defines a `pub fn area`, then any other file in the package can `import geometry` and call `geometry.area(...)`:

```silt
-- src/geometry.silt
pub fn area(w, h) {
  w * h
}

fn helper(x) {
  x * 2
} -- private: only visible inside geometry.silt
```

```silt
-- src/main.silt — anywhere in this file, `geometry.area(3, 4)` now resolves.
import geometry
```

See [Modules](language/modules.md) for the full rules.

## 6. Errors as values

No exceptions. Fallible functions return `Result`. The `?` operator propagates errors when the
surrounding function returns the same `Err` type:

```silt
import io

fn read_head(path) {
  let content = io.read_file(path)?
  Ok(content)
}
```

Use `match` to handle the result. Every stdlib error enum (here `IoError`) implements the built-in
`Error` trait, so you can pattern-match on specific variants and fall back to `.message()` for
anything else:

```silt
match io.read_file("app.json") {
  Ok(content) -> println("loaded {string.length(content)} bytes")
  Err(io.IoNotFound(path)) -> println("file does not exist: {path}")
  Err(e) -> println("error: {e.message()}")
}
```

## 7. Pipes and trailing closures

The `|>` operator passes the left value as the first argument of the right:

```silt
import list

[1, 2, 3, 4, 5]
|> list.filter { n -> n > 2 }
|> list.map { n -> n * n }
|> list.fold(0) { acc, n -> acc + n }
```

## 8. Concurrency

Spawn lightweight tasks that run in parallel. Communicate through channels. I/O inside tasks transparently yields — no async/await.

```silt
import channel
import task

fn main() {
  let ch = channel.new(10)

  let worker = task.spawn { -> channel.each(ch) { msg -> println("got: {msg}") } }

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
silt init                  -- create a new silt package in this directory
silt lsp                   -- start the language server
silt disasm <file.silt>    -- show bytecode disassembly (same as `silt run --disassemble`)
silt self-update           -- update the silt binary to the latest release
silt update [<dep-name>]   -- regenerate silt.lock for the current package's dependencies
silt add <name> --path <path>  -- add a path-based dependency to silt.toml
silt add <name> --git <url> [--rev|--branch|--tag <ref>]  -- add a git-based dependency to silt.toml
```

The `--watch` / `-w` flag works with `run`, `check`, `disasm`, and `test` to re-run the command when a file of the program (a module it imports, `silt.toml` or `silt.lock`) changes. A run that is still going, such as a server, is stopped first.

`silt check` checks a file as a module: a file needs no `main` to check (a library module, a file of types and traits, a test file), and a `main` it has must take no parameters. `silt run` starts a program at `main`, so it asks for one.

`silt check` reports what is wrong with the whole file: a declaration that does not parse is one error, and the declarations behind it are read and checked all the same, as is the text behind a character that belongs to no token. At most fifty syntax errors of a file are shown, with a count of the rest. A file that ends inside a string or a `{- -}` comment that is not closed reports that and nothing else. A file with a syntax error is never run or formatted.

`silt repl` checks each input as `silt check` checks a file, and runs it. It sees the directory it was started in as a script does: `import shapes` loads `shapes.silt` beside it, or `src/shapes.silt` and the dependencies of the package there. An input sees what the inputs before it define; an input with an error is dropped and changes nothing. Defining a function or a `let` again makes a new definition for the inputs after it: code from earlier inputs keeps the definition it was written against. Errors are shown at `<repl:n>:line:col`, the `n`th input. A module is read the first time an input imports it and kept for the session: edits to its file are seen after a restart. In a package, the REPL updates `silt.lock` as `silt run` does.

### Staying up to date

Run `silt self-update` to replace the installed binary with the latest GitHub release. It detects your platform, fetches the prebuilt archive, verifies it against the release's SHA-256 checksum, and atomically swaps the binary in place — no need to re-run the install script. Verification is fail-closed: a mismatch or missing `SHA256SUMS` file aborts the update without touching the installed binary. Pass `--dry-run` to preview the version that would be installed, or `--force` to reinstall when already current.

The bare `silt update` command regenerates `silt.lock` from the current package's `silt.toml`. Use `silt self-update` for binary self-updates.

## What's next

- **[Language Guide](language-guide.md)** — complete coverage of every feature
- **Standard Library** — every built-in function, constant, and type carries
  its docs through the LSP. Hover `list.map`, `math.cos`, `Result`, …
  in your editor to see signature, prose, and runnable examples.
- **[Concurrency](concurrency.md)** — the full CSP model, channels, and select
- **[FFI Guide](ffi.md)** — embed silt in Rust applications
- **[Editor Setup](editor-setup.md)** — configure your editor for silt
