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
silt test
```

`silt init` creates a `silt.toml` manifest, a starter `src/main.silt` and a first test, `src/main_test.silt`. `silt run` (with no arguments, inside the package directory) executes the package's entry point, and `silt test` runs the test. That's the whole loop. (`silt init --lib` creates a library instead, `src/lib.silt` with its test `src/lib_test.silt`, for other packages to import.)

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

A package that other packages use is a library: what they get from
`import <name>` is its `src/lib.silt`, the one module a dependency
exposes, and it needs no `src/main.silt`. A package declares a
dependency with `silt add <name> --path <path>`, or with
`silt add <name> --git <url> --tag <tag>` (a git dependency names its
tag, `--branch <branch>` or `--rev <commit>`).

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

`main` may return a `Result` too, and use `?`. An `Err` that reaches the end
of `main` is the program's message to its user: `silt run` writes it as one
line, `error: <e>`, and the program exits with status 1.

```silt
-- noexec
import io

fn main() -> Result((), io.IoError) {
  let content = io.read_file("app.json")?
  println(content)
  Ok(())
}
```

Use `match` to handle the result. Every stdlib error enum (here `IoError`) is shown as its
message, so you can pattern-match on specific variants and show anything else with `"{e}"`:

```silt
import io
import string

fn main() {
  match io.read_file("app.json") {
    Ok(content) -> println("loaded {string.length(content)} characters")
    Err(io.NotFound(path)) -> println("file does not exist: {path}")
    Err(e) -> println("error: {e}")
  }
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
silt run <dir>             -- run the package in that directory (its src/main.silt)
silt run <file.silt> -- a b   -- run it with the arguments a and b (io.args())
silt run -w <file.silt>    -- run and re-run on file changes
silt check <file.silt>...  -- type-check without running, each file on its own
silt check -               -- type-check the program read from stdin
silt check --format json <file.silt>   -- type-check with JSON output (for CI/editors)
silt check --deny-warnings <file.silt>   -- also fail on warnings (for CI)
silt test [path]           -- run test functions
silt fmt [files...]        -- format source code
silt fmt -                 -- format the program read from stdin, to stdout
silt fmt --check           -- check formatting without modifying files
silt repl                  -- interactive REPL
silt init [--lib]          -- create a new silt package (a library with --lib) in this directory
silt lsp                   -- start the language server
silt disasm <file.silt>    -- show bytecode disassembly
silt self-update           -- update the silt binary to the latest release
silt update                -- regenerate silt.lock for the current package's dependencies
silt add <name> --path <path>  -- add a path-based dependency to silt.toml
silt add <name> --git <url> (--rev|--branch|--tag) <ref>  -- add a git-based dependency to silt.toml
silt remove <name>         -- remove a dependency from silt.toml and silt.lock
```

A path given to `run`, `check`, `test`, `fmt` or `disasm` is a `.silt` file or a directory, and a directory that holds a `silt.toml` is that package: `silt check app` checks the package `app` as `silt check` inside it does, and `silt disasm app` shows its `src/main.silt`. Another directory stands for the `.silt` files under it (for `test`, its test files); `run` and `disasm` need one program, so they take only a package's directory. Any other file is refused: `silt check notes.txt` is an error, not a check of the text as silt.

The `--watch` / `-w` flag works with `run`, `check`, `disasm`, and `test` to re-run the command when a file of the program (a module it imports, `silt.toml` or `silt.lock`) changes. A run that is still going, such as a server, is stopped first.

`silt check -` and `silt fmt -` read the program from stdin, as an editor that pipes a buffer through them needs: `silt fmt -` writes the formatted program to stdout (with `--check`, nothing, and exit 1 when it is not formatted), and `silt check -` reports what it finds at `<stdin>`. The program is a file of the working directory: it imports the modules beside it and belongs to the package there. `-` takes no other file with it.

`silt fmt` changes the layout of a file and, beside it, only these spellings, each of which the checker reads the same:

- parentheses that group nothing and trailing commas go; a closure that is the last argument is written behind the call (in a `match` or `loop` header, inside the parentheses);
- imports move to the top of the file, sorted;
- `(a | b) | c` in a pattern is written `a | b | c`, and a closure's or a `match` arm's body of one expression in braces of its own is written without them;
- `xs |> f()` is written `xs |> f`;
- a `return x` on the last line of a function is written `x`;
- a last line `()` behind a `let` in a function's body is left out;
- `-> ()` of a function at the top level is left out when the body's value is `()` by its form (the body is empty, or its last line is a `let`, `()` or `return ()`) and the signature names no type variable, or a parameter has no type. Anywhere else `-> ()` stays: it is a return type, like `-> Int`, that the checker holds the function to, and leaving it out could change what the checker says (a body that ends in a call might return a value by mistake; with a type variable, the signature is what lets the body call the function at another type). The methods of traits and impls keep theirs.

A comment at a token that is left out stays, next to where the token was.

`silt check` checks a file as a module: a file needs no `main` to check (a library module, a file of types and traits, a test file), and a `main` it has must take no parameters. `silt run` starts a program at `main`, so it asks for one.

`silt check` reports what is wrong with the whole file: a declaration that does not parse is one error, and the declarations behind it are read and checked all the same, as is the text behind a character that belongs to no token. At most fifty syntax errors of a file are shown, with a count of the rest. A file that ends inside a string or a `{- -}` comment that is not closed reports that and nothing else. A file with a syntax error is never run or formatted.

Some problems are warnings, not errors: an import that nothing uses, a name that nothing reads, a local that hides an import. A warning never stops a program: `silt run`, `silt test` and the REPL show it and go on. Every command shows the warnings of every module that the program or its tests import, each once, and never a dependency's. A warning's help line names its fix, and changing the code is the only way to silence it. In a package, `silt check` checks every module in `src/`, imported or not, and the test files (`*_test.silt`), so one command sees every error and warning, and `silt check --deny-warnings` exits with 1 when there is one: that is the step for CI. `silt test` checks the package the same way before it runs a test (also when the path it is given is in the package), and fails when the package has an error, even one that no test file imports.

`silt repl` checks each input as `silt check` checks a file, and runs it. The REPL runs an input as soon as it is complete, so a line that starts with `|>` is a new input: end the line before it with `|>`, or put the pipeline in parentheses. It sees the directory it was started in as a script does: `import shapes` loads `shapes.silt` beside it, or `src/shapes.silt` and the dependencies of the package there. An input sees what the inputs before it define; an input with an error is dropped and changes nothing. Defining a function or a `let` again makes a new definition for the inputs after it: code from earlier inputs keeps the definition it was written against. Errors are shown at `<repl:n>:line:col`, the `n`th input. A module is read the first time an input imports it and kept for the session: edits to its file are seen after a restart. In a package, the REPL updates `silt.lock` as `silt run` does. `:type <expr>` shows the type of an expression without running it (`:type Some` is `Fn(a) -> Option(a)`; its expression is checked as an input, and counts as one), and `:doc <name>` what a builtin is: a function's signature, the declaration of a type, a trait or a variant's enum, with what its docs say first, or all a module declares (`:doc list.map`, `:doc Option`, `:doc string`); `:help` lists the commands.

### Staying up to date

Run `silt self-update` to replace the installed binary with the latest GitHub release. It detects your platform, fetches the prebuilt archive over https, verifies it against the release's SHA-256 checksum and, when the GitHub CLI (`gh`, 2.49 or later) is installed and logged in to github.com, against the build attestation the release workflow signed, and atomically swaps the binary in place — no need to re-run the install script. Verification is fail-closed: a mismatch, a missing `SHA256SUMS` file or an attestation that does not verify aborts the update without touching the installed binary. Pass `--dry-run` to preview the version that would be installed, or `--force` to reinstall when already current.

The bare `silt update` command regenerates `silt.lock` from the current package's `silt.toml`. Use `silt self-update` for binary self-updates.

`silt check`, `silt run` and `silt test` fetch the git dependencies of `silt.toml` that are not in silt's cache. The language server fetches nothing: opening a folder in an editor runs no git and connects to no host. A git dependency that is not fetched yet is an error on its line of `silt.toml` in the editor, "not fetched: run `silt check`"; once `silt check` has fetched it, the editor reads it from the cache.

`silt.lock` pins each git dependency to a commit and to a checksum of its sources (`silt.toml` and `src/`). Every command that loads the dependency compares its checkout with that checksum and refuses one that differs, naming the dependency, both checksums and the checkout in silt's cache: a checkout that was changed after it was fetched never runs. Delete that checkout to fetch the commit again. `silt update` takes nothing from the cache: it fetches every git dependency afresh and pins what it fetched. A path dependency has no checksum in `silt.lock`: its sources change as you work on them. A path dependency, and a git dependency given by a relative path (`git = "../repo"`), is written as a path relative to the directory of `silt.toml`, so a package moved or checked out elsewhere together with its dependencies keeps its `silt.lock`.

## What's next

- **[Language Guide](language-guide.md)** — complete coverage of every feature
- **Standard Library** — every built-in function, constant, and type carries
  its docs through the LSP. Hover `list.map`, `math.cos`, `Result`, …
  in your editor to see signature, prose, and runnable examples.
- **[Concurrency](concurrency.md)** — the full CSP model, channels, and select
- **[FFI Guide](ffi.md)** — embed silt in Rust applications
- **[Editor Setup](editor-setup.md)** — configure your editor for silt
