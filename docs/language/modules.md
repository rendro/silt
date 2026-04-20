---
title: "Modules"
section: "Language"
order: 8
---

# Modules

**File = module.** Each `.silt` file is a module named after the file:

```silt
-- File: math.silt
pub fn add(a, b) = a + b
fn internal_helper(x) = x * 2   -- private
```

**Private by default.** Only `pub` items are exported. When a `pub type` has
enum variants, all constructors are exported too.

**Three import forms:**

```silt
import math                  -- qualified: math.add(1, 2)
import math.{ add, Point }   -- direct: add(1, 2)
import math as m              -- aliased: m.add(1, 2)
```

**Built-in modules** are registered in the global environment (no `.silt`
files needed). See [`docs/stdlib/index.md`](../stdlib/index.md) for the
authoritative list of modules and their key functions.

## Notable Standard Library Details

- `float.round`, `float.ceil`, `float.floor` return **`Float`**, not `Int`.
  Use `float.to_int` to convert after rounding.
- `float.to_string` is overloaded: the 1-arg form `float.to_string(f)` returns
  the shortest round-trippable representation, while the 2-arg form
  `float.to_string(f, decimals)` formats with a fixed decimal count. Both
  accept `Float` and `ExtFloat` values.
- `string.is_empty(s)` checks for zero-length strings.
- Character classification: `string.is_alpha`, `string.is_digit`,
  `string.is_upper`, `string.is_lower`, `string.is_alnum`,
  `string.is_whitespace`.
- `regex.replace_all_with(pattern, text, fn)` takes a callback for per-match
  replacement.
- The `time` module provides `Instant`, `Date`, `Time`, `DateTime`, `Duration`,
  and `Weekday` types with nanosecond precision. Comparison operators (`<`, `>`)
  work correctly on all time types. Display shows ISO 8601 format.
- The `http` module provides `Method` (enum: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`,
  `HEAD`, `OPTIONS`), `Request`, and `Response` record types. `http.get` and
  `http.request` return `Result(Response, String)`. `http.serve` takes a port
  and a handler function `Fn(Request) -> Response`. Pattern matching on
  `(req.method, segments)` replaces routing DSLs.

Circular imports are detected and rejected with a clear error message.
