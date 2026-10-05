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

A module path is one name: there is no `import net.http`, and a file in a
subdirectory of `src/` is not a module.

```
src/
  main.silt
  geometry.silt      -- imported as `geometry`
```

## Visibility

Items are **private by default**. Only `pub` items are exported:

```silt
pub fn add(a, b) { a + b }
fn helper(x) { x * 2 }       -- not exported

pub let limit = 10                  -- exported
pub let (low, high) = (1, 99)       -- exports `low` and `high`

pub type Point { x: Int, y: Int }   -- exports the type and its constructor
pub type Shape {                     -- exports the type and all variants
  Circle(Int),
  Square(Int),
}
```

When a `pub type` declares enum variants, all constructors are exported with
it. A trait is exported with `pub trait`:

```silt
pub trait Describe {
  fn describe(self) -> String
}
```

The methods of a trait declared without `pub` can be called only inside
its module, even on a value of an exported type.

A public declaration cannot name a private record, enum or trait of its
module: a `pub fn` whose parameter, return type or `where` bound, or a
`pub type` whose field or variant, names one is an error, since an
importer could use the declaration but never name what it uses. A
private type alias may appear there: an alias is only another name for
its type.

An impl is never exported or imported: `trait Describe for Point { ... }`
applies wherever the trait and the type are used.

Naming a private item from another module is an error at the name:
`import geometry.{ helper }` reports that `helper` is private to
`geometry`, and `import geometry.{ nope }` that `geometry` has no member
`nope`.

## Imports

Three forms:

```silt
import geometry                   -- qualified:  geometry.add(1, 2)
import geometry.{ add, Point }    -- direct:     add(1, 2)
import geometry as g              -- aliased:    g.add(1, 2)
```

`import geometry` binds one name, `geometry`, and every member of the
module is reached through it, in every position:

- functions and values: `geometry.add(1, 2)`;
- constructors and record literals: `geometry.Circle(2.0)`,
  `geometry.Shape.Circle(2.0)`, `geometry.Point { x: 1, y: 2 }`;
- patterns: `geometry.Circle(r) ->`, `geometry.Shape.Circle(r) ->`,
  `geometry.Point { x, .. } ->`;
- types: `fn area(s: geometry.Shape)`, `List(geometry.Point)`,
  `Fn(geometry.Point) -> Int`, `type Shapes = List(geometry.Shape)`;
- traits: `trait geometry.Describe for Local`, `trait Display for geometry.Point`,
  `where a: geometry.Describe`, `trait Loud: geometry.Describe`.

`geometry.Circle` works when `Circle` is the only variant of that name
among the module's exports; `geometry.Shape.Circle` always works.

`import geometry.{ add, Shape }` binds exactly `add` and `Shape`, and not
`geometry`. An enum imported this way does not bring its variants: write
`Shape.Circle(2.0)`, or list `Circle` as well. To also use other items as
`geometry.sub`, add a separate `import geometry`.

A member used without its module is an error that says how to reach it:

```
error[type]: undefined variable 'add'
  = help: did you mean `geometry.add`? or import the name: `import geometry.{ add }`
```

### Variants of one name

Two enums may have variants of one name. `Shape.Red` and `Color.Red` say
which; a bare `Red` where both enums are in scope is an error, with a
label at each declaration:

```silt
type Shape { Red, Square }
type Color { Red, Blue }

fn main() {
  let s = Shape.Red
  let c = Color.Red
  println("{s} {c}")     -- Red Red
}
```

From another module, `geometry.Red` works when `Red` is the only variant of
that name among the module's exports, and `geometry.Shape.Red` always
works. `import geometry.{ Red }` of a name two of its enums share is an
error: import the enum and write `Shape.Red`.

### Two types of one name

Two modules may each declare a type of one name. They are different
types, kept apart by their qualifiers: `a.Pt` and `b.Pt`, or a module's
own `Pt` and an imported `b.Pt`. A value of one is not a value of the
other, and each has its own trait impls.

A value of such a type prints qualified, as the program names the
module: `a.Pt {x: 1}` for a module `a` of the program, `db.Pt {x: 1}`
for the library of a dependency `db`, and `db.util.Pt {x: 1}` for its
module `util`. A type whose name no other type of the program has
prints bare: `Pt {x: 1}`.

Two modules may also each declare a trait of one name, say `Show` with
a method `show`, and implement it for one type. A method call means the
method of the trait its module sees: a trait it declares, imports by
name, or reaches through a module it imports. A call that sees both
traits is an error at the call. The builtin traits (`Display`,
`Compare`, `Equal`, `Hash`, `Error`) are always seen: a trait of the
program with a method named `display` implemented for `Int` makes
`5.display()` ambiguous; call it through a `where` bound for the
trait. A derived impl always calls the builtin trait's methods of its
fields.

## Module names and shadowing

Module names follow the ordinary lexical-scoping rules: a value binding
inside a function (function parameter, lambda parameter, `let`, pattern
binder) with the same name **shadows** an imported module within its scope.
At the top level there is no shadowing: an import and a top-level `fn`,
`type`, `let` or trait may not bind the same name, and neither may two
imports (`import a.{ x }` and `import b.{ x }`, or two `import m as n` with
the same alias). Such a program is rejected with an error naming both
declarations.

```silt
import other            -- other.silt: pub fn double(x) { x * 2 }

type P { year: Int }

fn f(other: P) -> Int {
  other.year            -- field access on the parameter, not a module lookup
}

fn caller() -> Int {
  other.double(21)      -- no `other` binding in scope here: module call
}
```

(The compiler emits a warning when a binding shadows a *builtin* module,
since the module's functions become unreachable inside that scope.)

Type names are not value bindings, so `Shape.Circle` keeps resolving through
the enum even though `Shape` is in scope. Qualified **type** paths
(`geometry.Point { .. }` literals, `geometry.Circle(r)` patterns) are an
exception in the other direction: because a local can never carry that
syntax, using a module qualifier whose name is shadowed by a value binding
is a compile error rather than a silent module pick — rename the binding or
the import.

## Multi-file projects

`silt init` creates a package with a `silt.toml` manifest and a `src/` tree.
The entry point is `src/main.silt`, and every `.silt` file directly in
`src/` is a module of the package, imported by its file name.

External dependencies are declared in `silt.toml` via `silt add <name>
--path <path>` or `silt add <name> --git <url>`. After adding, imports from
the dependency package work exactly like local modules.

`import x` in a module of a package means, in this order: the builtin
module `x`; the dependency whose key under `[dependencies]` in this
package's `silt.toml` is `x` (its `src/lib.silt`); this package's own
`src/x.silt`. A dependency's own dependencies are its own: to import one,
declare it in your `silt.toml` too. A module file named like a builtin
module (`src/list.silt`) is an error, because `import list` always means
the builtin one.

A package consumed as a dependency exposes `src/lib.silt` instead of
`src/main.silt`. Such a library-only package has nothing to execute — `silt
run` refuses it — but a bare `silt check` works, falling back to
`src/lib.silt` when no `src/main.silt` exists.

## Built-in modules

Standard-library modules are registered in the global environment — there is
no `.silt` file for them. You still import them explicitly:

```silt
import io
import list
import channel
```

Each built-in module has a reference page under
[`docs/stdlib/`](../stdlib/), and the LSP shows the same text: hover
any qualified built-in name (`list.map`, `math.cos`, `Result`, …) in
your editor. The built-in modules:

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

The types and enums of a built-in module are its members like any
other: `time.Weekday` and `time.Monday`, `channel.Message(v)` and
`channel.Closed`, `http.Request` and `http.GET`, `io.IoError` and
`io.IoNotFound(path)`, `list.Stop(acc)`, `tcp.TcpStream`,
`task.Handle(a)`, `postgres.PgPool` (and `PgTx`, `PgCursor`,
`QueryResult`, `ExecResult`, `Value`). `ParseError`, the error of both
`int.parse` and `float.parse`, is declared in `int` and reached through
either module: `int.ParseError` and `float.ParseError` are one type. A
selective import works for them too: `import channel.{ Message }`.

The **prelude** needs no import: the primitive and container types (`Int`,
`Float`, `Bool`, `String`, `Bytes`, `List`, `Map`, `Set`, `Channel`, ...),
`TypeOf(a)` (the type of a type used as a value, like `Int` or a
`type a` parameter), `Option`, `Result`, `Some`, `None`, `Ok`, `Err`, `print`, `println` and
`panic`. A module's own declaration or import of one of these names
shadows the prelude: after `type Maybe { Some(a), None }`, a bare `None`
is `Maybe.None`, and the prelude's is still `Option.None`.


## Circular imports

silt **rejects circular imports** at compile time. If `a.silt` imports `b`
which imports `a`, the compiler emits the full chain:

```
error[compile]: circular import detected: a -> b -> a (...)
```

Cycles inside a single package render with bare module names; cycles that
cross package boundaries use the qualified `package::module` form so the
boundary is visible. Break the cycle by moving the shared code into a third
module that both sides import.
