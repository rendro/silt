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
-- File: src/math.silt
pub fn add(a, b) { a + b }
fn helper(x) { x * 2 }   -- private
```

A file in a subdirectory is imported with a dotted path:

```
src/
  main.silt
  math.silt          -- imported as `math`
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
  Circle(Int)
  Square(Int)
}
```

When a `pub type` declares enum variants, all constructors are exported with
it.

## Imports

Three forms:

```silt
import math                   -- qualified:  math.add(1, 2)
import math.{ add, Point }    -- direct:     add(1, 2)
import math as m              -- aliased:    m.add(1, 2)
```

`import math.{ add }` brings only `add` into scope. To also use other items
as `math.sub`, add a separate `import math`.

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

See the [Standard Library Index](../stdlib/index.md) for the full list and
each module's reference.

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
