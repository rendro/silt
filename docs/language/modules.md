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
files needed):

| Module    | Key Functions                                                |
|-----------|--------------------------------------------------------------|
| `io`      | `inspect`, `read_file`, `write_file`, `read_line`, `args`   |
| `list`    | `map`, `filter`, `fold`, `each`, `find`, `zip`, ...         |
| `map`     | `get`, `set`, `delete`, `contains`, `keys`, `values`, ...   |
| `set`     | `new`, `from_list`, `to_list`, `contains`, `insert`, ...    |
| `string`  | `split`, `join`, `trim`, `contains`, `replace`, `length`    |
| `int`     | `parse`, `abs`, `min`, `max`, `to_float`, `to_string`       |
| `float`   | `parse`, `round`, `ceil`, `floor`, `abs`, `to_int`, `to_string` |
| `result`  | `is_ok`, `is_err`, `map_ok`, `map_err`, `unwrap_or`, `flatten`, `flat_map` |
| `option`  | `is_some`, `is_none`, `map`, `unwrap_or`, `to_result`, `flat_map` |
| `regex`   | `is_match`, `find`, `find_all`, `replace`, `replace_all_with` |
| `json`    | `parse`, `stringify`, `pretty`                               |
| `test`    | `assert`, `assert_eq`, `assert_ne`                           |
| `channel` | `new`, `send`, `receive`, `close`, `select`, `each`         |
| `task`    | `spawn`, `join`, `cancel`                                    |
| `time`    | `now`, `today`, `date`, `format`, `parse`, `add_days`, `weekday`, `sleep` |
| `fs`      | `exists`, `is_file`, `is_dir`, `list_dir`, `mkdir`, `remove`, `copy`, `rename` |
| `http`    | `get`, `request`, `serve`, `segments`                        |

## Notable Standard Library Details

- `float.round`, `float.ceil`, `float.floor` return **`Float`**, not `Int`.
  Use `float.to_int` to convert after rounding.
- `float.to_string(f, decimals)` takes **two arguments** -- no overloading.
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
