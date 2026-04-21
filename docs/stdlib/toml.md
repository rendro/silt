---
title: "toml"
section: "Standard Library"
order: 11
---

# toml

Parse TOML documents into typed silt values and serialize values to TOML.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `parse` | `(String, type a) -> Result(a, String)` | Parse a TOML document (top-level table) into a record |
| `parse_list` | `(String, type a) -> Result(List(a), String)` | Parse a single `[[items]]` section into a list of records |
| `parse_map` | `(String, type v) -> Result(Map(String, v), String)` | Parse a top-level table into a map |
| `pretty` | `(a) -> Result(String, String)` | Pretty-print a value as TOML |
| `stringify` | `(a) -> Result(String, String)` | Serialize a value as TOML |


## `toml.parse`

```
toml.parse(s: String, type a) -> Result(a, String)
```

Parses a TOML document into a record of type `a`. The type is passed as a
`type` parameter (see [Generics](../language/generics.md) — not a string).
Fields are matched by name; `Option` fields default to `None` if missing
from the document.

Fields of type `Date`, `Time`, and `DateTime` (from the `time` module) accept
either a TOML native datetime literal or an ISO 8601 string — the same formats
that `json.parse` accepts for each of those field types:

| Field type | Accepted formats | Example |
|------------|-----------------|---------|
| `Date` | `YYYY-MM-DD` | `1979-05-27` or `"1979-05-27"` |
| `Time` | `HH:MM:SS`, `HH:MM` | `07:32:00` or `"07:32:00"` |
| `DateTime` | RFC 3339 / ISO 8601, with optional `Z` or `±HH:MM` offset | `1979-05-27T07:32:00Z` |

```silt
import toml
type User {
    name: String,
    age: Int,
    active: Bool,
}

fn main() {
    let input = "name = \"Alice\"\nage = 30\nactive = true\n"
    match toml.parse(input, User) {
        Ok(user) -> println(user.name)
        Err(e) -> println("Error: {e}")
    }
}
```


## `toml.parse_list`

```
toml.parse_list(s: String, type a) -> Result(List(a), String)
```

TOML's spec requires the top level of every document to be a table, so there is
no direct equivalent of a JSON top-level array. `toml.parse_list` therefore
accepts a document whose top-level table contains exactly one key — an array-of-
tables — and returns the list of records under that key. This is the natural
shape of `[[items]]` sections:

```silt
import toml
import list
type Point { x: Int, y: Int }

fn main() {
    let input = "[[points]]\nx = 1\ny = 2\n\n[[points]]\nx = 3\ny = 4\n"
    match toml.parse_list(input, Point) {
        Ok(points) -> list.each(points) { p -> println("{p.x}, {p.y}") }
        Err(e) -> println("Error: {e}")
    }
}
```

The key name (`points` in the example above) is not checked — any single
top-level array-of-tables key works.


## `toml.parse_map`

```
toml.parse_map(s: String, type v) -> Result(Map(String, v), String)
```

Parses a top-level TOML table into a `Map(String, v)`. The type is passed as
a `type` parameter (`Int`, `Float`, `String`, `Bool`, or a record type).

```silt
import toml
import map
fn main() {
    let input = "x = 10\ny = 20\n"
    match toml.parse_map(input, Int) {
        Ok(m) -> println(map.get(m, "x"))  -- Some(10)
        Err(e) -> println("Error: {e}")
    }
}
```


## `toml.pretty`

```
toml.pretty(value: a) -> Result(String, String)
```

Serialises a silt record or map to a multi-line, human-friendly TOML document.
TOML's default output is already readable (one key per line, nested tables as
`[section]` headers), so `toml.pretty` differs only slightly from `toml.stringify`
— nested arrays of tables render as `[[section]]` blocks instead of inline
arrays.

Note: the top-level value must be a record or a map; TOML cannot represent a
bare scalar or array at top level.

```silt
import toml
fn main() {
    let data = #{"name": "silt", "version": "1.0"}
    match toml.pretty(data) {
        Ok(s) -> println(s)
        Err(e) -> println("Error: {e}")
    }
}
```


## `toml.stringify`

```
toml.stringify(value: a) -> Result(String, String)
```

Serialises a silt record or map to a TOML document. Like `toml.pretty`, the
top-level value must be table-shaped.

```silt
import toml
type Package { name: String, version: String }
fn main() {
    let data = Package { name: "silt", version: "1.0" }
    match toml.stringify(data) {
        Ok(s) -> println(s)
        Err(e) -> println("Error: {e}")
    }
}
```
