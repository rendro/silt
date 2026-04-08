---
title: "Bindings and Functions"
---

# Bindings and Functions

## Philosophy

### 14 Keywords

Silt has exactly 14 keywords:

```
as  else  fn  import  let  loop  match  mod
pub  return  trait  type  when  where
```

This is a forcing function, not an aesthetic choice. Every time we considered
adding a keyword (`if`, `for`, `while`, `mut`, `async`, `await`,
`catch`, `throw`...), we asked: "Can an existing construct handle this?" The
answer was almost always yes.

- `if`/`else` is subsumed by `match`.
- General-purpose iteration uses `loop`, collection traversal uses
  higher-order functions (`list.map`, `list.filter`, `list.fold`).
- `mut` does not exist because nothing is mutable.
- `async`/`await` does not exist because concurrency is CSP-based.
- `try`/`catch` does not exist because errors are values.

Concurrency primitives live in modules (`channel.new`, `channel.send`,
`task.spawn`, etc.) rather than as keywords — this keeps the global
namespace clean and avoids the PHP problem of too many bare globals.

The global namespace has only 12 names: `print`, `println`, `panic`,
`Ok`, `Err`, `Some`, `None`, `Stop`, `Continue`, `Message`, `Closed`,
`Empty`. Everything else requires module qualification.

What is _not_ a keyword matters too. `true`/`false` are builtin literals.
`Ok`, `Err`, `Some`, `None` are builtin variant constructors -- ordinary
values defined in the prelude. `_` is a wildcard pattern token. This keeps
the keyword count honest.

### Expression-Based

Every construct in Silt is an expression. A `match` returns a value. A block
returns its last expression. There are no "statements" -- `let` and `when`
are statement-level forms inside blocks, but blocks themselves are expressions.

```silt
let description = match shape {
  Circle(r) -> "circle with radius {r}"
  Rect(w, h) -> "rect {w}x{h}"
}

let result = {
  let x = compute()
  let y = transform(x)
  x + y
}
```

The trade-off: functions that exist only for side effects return `()` (Unit).

### Immutability as Default (and Only Option)

All bindings are immutable. There is no `mut`, no mutable references, no
assignment to existing bindings. Shadowing is allowed:

```silt
let x = 42
let x = x + 1    -- shadowing, not mutation
```

Why no mutation at all? (1) Concurrency safety — immutable values need no
locks. (2) Simpler reasoning — values never change after creation.

The trade-off is real: algorithms that naturally use mutation (in-place
sorting, graph traversal with visited sets) require recursion or functional
combinators. Record update syntax (`user.{ age: 31 }`) is the mitigation —
it looks like mutation but always returns a new value.

### Explicit Over Implicit

Silt has no exceptions, no null, no implicit conversions, no implicit error
propagation. `1 + 1.0` is a type error. If a function can fail, its return
type says so. If a value might be absent, its type says so. If control flow
can exit early, the syntax (`?` or `when`-`else`) says so.

### One Way to Do Things

`match` subsumes `if`. `loop` subsumes `while`. String interpolation
`"{a}{b}"` subsumes concatenation. Module-qualified functions subsume bare
globals. When there is one way, every Silt program reads the same way.


## Language Features

### Bindings

Every value is bound with `let`. No `var`, no `mut`, no reassignment:

```silt
let x = 42
let name = "Robert"
```

**Shadowing** creates a new binding with the same name:

```silt
let x = 1
let x = x + 1   -- x is now 2; the original 1 is untouched
```

**Destructuring** works in `let` using the same pattern language as `match`:

```silt
let (x, y) = (1, "hello")
let [a, b, c] = [1, 2, 3]
let User { name, age, .. } = user
```

**Type annotations** are optional (Hindley-Milner infers everything) but
useful for documentation:

```silt
let x: Int = 42
let transform: Fn(Int) -> Int = fn(x) { x * 2 }
```


### Functions

**Named functions** use block bodies. The last expression is the return value:

```silt
fn add(a, b) {
  a + b
}
```

**Single-expression shorthand** uses `=`:

```silt
fn square(x) = x * x
fn greet(name) = "hello {name}"
```

**Anonymous functions (closures)** are values that close over their environment:

```silt
let double = fn(x) { x * 2 }

fn make_adder(n) {
  fn(x) { x + n }
}
```

**No nested named functions.** Use `let f = fn(x) { ... }` for local helpers.
Named functions are always top-level, keeping scoping rules simple.

**Trailing closures:** when the last argument is a closure, write it outside
the parentheses:

```silt
[1, 2, 3] |> list.map { x -> x * 2 }
[1, 2, 3] |> list.fold(0) { acc, x -> acc + x }

-- Destructuring in closure parameters
pairs |> list.each { (n, word) -> println("{n} is {word}") }
```

**Return type annotations:**

```silt
fn add(a: Int, b: Int) -> Int {
  a + b
}
```

**Early return** with `return`. Both `return` and `panic()` produce the
`Never` type, which unifies with any other type -- so they can appear in any
expression position without causing type errors:

```silt
fn get_or_die(opt) {
  match opt {
    Some(v) -> v
    None -> panic("expected a value")   -- Never unifies with v's type
  }
}
```
