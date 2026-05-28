---
title: "Modules"
section: "Language"
order: 9
---

# Modules

silt's module system maps directly to the filesystem. There is no `module` or
`package` keyword inside source files — the name comes from the path.

## File = module

Each `.silt` file is a module named after the file:

```silt
-- File: src/geometry.silt
pub fn add(a, b) { a + b }
fn helper(x) { x * 2 }   -- private
```

A file in a subdirectory is imported with a dotted path:

```
src/
  main.silt
  geometry.silt      -- imported as `geometry`
  net/
    http.silt        -- imported as `net.http`
```

## Visibility

Items are **private by default**. Only `pub` items are exported:

```silt
pub fn add(a, b) { a + b }
fn helper(x) { x * 2 }       -- not exported

pub type Point { x: Int, y: Int }   -- exports the type and its constructor
pub type Shape {                     -- exports the type and all variants
  Circle(Int),
  Square(Int),
}
```

When a `pub type` declares enum variants, all constructors are exported with
it.

## Imports

Three forms:

```silt
import geometry                   -- qualified:  geometry.add(1, 2)
import geometry.{ add, Point }    -- direct:     add(1, 2)
import geometry as g              -- aliased:    g.add(1, 2)
```

`import geometry.{ add }` brings only `add` into scope. To also use other
items as `geometry.sub`, add a separate `import geometry`.

## Multi-file projects

`silt init` creates a package with a `silt.toml` manifest and a `src/` tree.
The entry point is `src/main.silt`, and every `.silt` file under `src/` is a
module in the package. Modules in subdirectories use dotted paths: `net/`,
`util/crypto/`, etc.

External dependencies are declared in `silt.toml` via `silt add <name>
--path <path>` or `silt add <name> --git <url>`. After adding, imports from
the dependency package work exactly like local modules.

## Built-in modules

Standard-library modules are registered in the global environment — there is
no `.silt` file for them. You still import them explicitly:

```silt
import io
import list
import channel
```

Stdlib documentation is delivered through the LSP — hover any
qualified built-in name (`list.map`, `math.cos`, `Result`, …) in your
editor to see the reference for that name. The list of built-in
modules is enumerated by `silt::module::BUILTIN_MODULES`:

| Module | Purpose |
| --- | --- |
| `io` | File I/O and stdout (`io.read_file`, `io.write_file`, `println`) |
| `string` | String inspection, slicing, and conversion helpers |
| `int` | Integer parsing, formatting, and bounded arithmetic |
| `float` | Floating-point parsing, classification, and numeric helpers |
| `list` | List construction, traversal, and transformation |
| `map` | Hash-map insertion, lookup, and iteration |
| `result` | `Result` combinators (`map_ok`, `map_err`, `unwrap_or`, …) |
| `option` | `Option` combinators (`map`, `unwrap_or`, `flat_map`, …) |
| `test` | Assertion harness used by `silt test` (`test.assert_eq`, …) |
| `channel` | Bounded MPMC channels for CSP-style messaging |
| `task` | Structured concurrency: `task.spawn`, `task.join`, `task.cancel`, `task.deadline`, `task.spawn_until` |
| `regex` | Compiled regular expressions and replacement helpers |
| `json` | Type-directed JSON parsing and emission |
| `toml` | TOML parsing and emission |
| `set` | Hash-set construction and bulk operations |
| `math` | Trigonometry, exponentials, and numeric constants |
| `time` | Instants, durations, calendar dates, and weekdays |
| `http` | HTTP client and server (`http.get`, `http.request`, `http.serve`, `http.serve_all`, `http.segments`, `http.parse_query`) |
| `fs` | Filesystem queries (`fs.list_dir`, `fs.stat`, `fs.read_link`, `fs.walk`, `fs.glob`, `fs.mkdir`, `fs.remove`, `fs.rename`, `fs.copy`, `fs.exists`, `fs.is_file`, `fs.is_dir`, `fs.is_symlink`) |
| `env` | Process environment access (`env.get`, `env.set`, `env.remove`, `env.vars`) |
| `postgres` | PostgreSQL client with typed parameters (opt-in feature; build with `cargo build --features postgres`, not enabled in the default release binary) |
| `bytes` | Byte-buffer construction, slicing, and conversion |
| `crypto` | Hashing, HMAC, and constant-time comparison |
| `encoding` | Hex / base64 / URL encoding helpers |
| `tcp` | TCP listener and stream primitives |
| `stream` | Lazy iterators backed by tasks and channels |
| `uuid` | UUID generation and parsing |

The order of rows matches the order of entries in `BUILTIN_MODULES`; a
parity-lock test in `tests/round74_modules_doc_lists_all_builtins_tests.rs`
asserts that every entry appears in this listing.

## Circular imports

silt **rejects circular imports** at compile time. If `a.silt` imports `b`
which imports `a`, the compiler emits the full chain:

```
error: circular import: a -> b -> a
```

Cycles inside a single package render with bare module names; cycles that
cross package boundaries use the qualified `package::module` form so the
boundary is visible. Break the cycle by moving the shared code into a third
module that both sides import.
