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
| `parse` | `(String, type a) -> Result(a, TomlError)` | Parse a TOML document (top-level table) into a record |
| `parse_list` | `(String, type a) -> Result(List(a), TomlError)` | Parse a single `[[items]]` section into a list of records |
| `parse_map` | `(String, type a) -> Result(Map(String, a), TomlError)` | Parse a top-level table into a map |
| `pretty` | `(a) -> Result(String, TomlError)` | Pretty-print a value as TOML |
| `stringify` | `(a) -> Result(String, TomlError)` | Serialize a value as TOML |

## Errors

Every fallible `toml.*` call returns `Result(T, TomlError)`. The `TomlError`
enum has four variants you can pattern-match on, or fall back to
`e.message()` when you just want a rendered string (`trait Error for
TomlError` is wired in):

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Syntax(msg, offset)` | `String`, `Int` | Malformed TOML at `offset` bytes |
| `TypeMismatch(expected, actual)` | `String`, `String` | A field's TOML type did not match the target field type |
| `MissingField(name)` | `String` | A required (non-`Option`) field was absent |
| `Other(msg)` | `String` | Anything else (serialization failures, document-shape violations) |

See [stdlib errors](errors.md) for the shared `Error` trait.


## Decodable types

The type argument of `toml.parse`, `toml.parse_list` and `toml.parse_map`
names the type to decode into:

| Function | Type argument |
|----------|---------------|
| `toml.parse` | a record type |
| `toml.parse_list` | a record type |
| `toml.parse_map` | `Int`, `Float`, `String`, `Bool`, or a record type |

The fields of a record type (and of every record type nested in it) may have
these types:

- `Int`, `Float`, `String`, `Bool`
- `Date`, `Time`, `DateTime`
- `List(T)`, `Option(T)`, `Map(String, T)`
- tuples, such as `(Int, String)`
- non-generic record types
- type aliases of any of these

where `T` is again one of these types. Anything else (a `Set`, an enum, a
generic record, a function, a `Map` whose key is not `String`, a type
parameter) has no decoder.

A TOML integer goes into an `Int`, and into a `Float` where one is
expected. A TOML float goes into a `Float` only: `x = 1.0` for an `Int`
field is `Err(TypeMismatch("Int", "float"))`, because TOML says
which kind a number is.

When the type is written at the call, `toml.parse(text, Config)`, the
compiler checks it. A record with a field no decoder exists for is a compile
error that names the field and its type; so is an enum, or a type the function
does not take (such as `toml.parse_list(text, Int)`). The check applies
equally to a decoder imported by name (`import toml.{ parse }`) and to one
called through a module alias. When the type only arrives at run time, through
a `type a` parameter, the same problems are reported then: a field without a
decoder gives an `Err(Other(msg))` whose message names the field and its
record, and a type the function does not take is a run-time error.


## Nesting limits

`toml.stringify` and `toml.pretty` write a value nested up to 1,000 levels
deep (the same bound as `json.stringify`). The element of a list or of a
tuple, the field of a record or of an enum variant and the value of a map are
each one level further in than what holds them; the content of a `Some` is
not (it is written in the `Some`'s place). A value nested deeper is an
`Err(Other(msg))`:

```text
toml.stringify: the value is nested more than 1000 levels deep
```

The parsers stop sooner: `toml.parse`, `toml.parse_list` and
`toml.parse_map` read a document whose arrays and tables are nested up to
79 levels deep, and give a `Syntax` error (`recursion limit exceeded`) for
one nested deeper. A value written at more levels than that is valid TOML
that silt itself does not read back.


## `toml.parse`

```
toml.parse(s: String, type a) -> Result(a, TomlError)
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
    Err(e) -> println("Error: {e.message()}")
  }
}
```


## `toml.parse_list`

```
toml.parse_list(s: String, type a) -> Result(List(a), TomlError)
```

TOML's spec requires the top level of every document to be a table, so there is
no direct equivalent of a JSON top-level array. `toml.parse_list` therefore
accepts a document whose top-level table contains exactly one key — an array-of-
tables — and returns the list of records under that key. This is the natural
shape of `[[items]]` sections:

```silt
import list
import toml

type Point {
  x: Int,
  y: Int,
}

fn main() {
  let input = "[[points]]\nx = 1\ny = 2\n\n[[points]]\nx = 3\ny = 4\n"
  match toml.parse_list(input, Point) {
    Ok(points) -> list.each(points) { p -> println("{p.x}, {p.y}") }
    Err(e) -> println("Error: {e.message()}")
  }
}
```

The key name (`points` in the example above) is not checked — any single
top-level array-of-tables key works.


## `toml.parse_map`

```
toml.parse_map(s: String, type a) -> Result(Map(String, a), TomlError)
```

Parses a top-level TOML table into a `Map(String, v)`. The type is passed as
a `type` parameter (`Int`, `Float`, `String`, `Bool`, or a record type).

```silt
import map
import toml

fn main() {
  let input = "x = 10\ny = 20\n"
  match toml.parse_map(input, Int) {
    Ok(m) -> println(map.get(m, "x")) -- Some(10)
    Err(e) -> println("Error: {e.message()}")
  }
}
```


## `toml.pretty`

```
toml.pretty(value: a) -> Result(String, TomlError)
```

Serialises a silt record or map to a multi-line, human-friendly TOML document.
TOML's default output is already readable (one key per line, nested tables as
`[section]` headers), so `toml.pretty` differs only slightly from `toml.stringify`
— nested arrays of tables render as `[[section]]` blocks instead of inline
arrays.

Note: the top-level value must be a record or a map; TOML cannot represent a
bare scalar or array at top level.

A value TOML has no form for, wherever in the value it is, is an
`Err(Other(message))` whose message names its kind: `Bytes`, a `Set`, a function
(the constructor of a variant, such as `Some`, is one), a channel, a task
handle, a TCP listener or stream.

```text
toml.stringify: unsupported value kind Fn
```

```silt
import toml

fn main() {
  let data = #{ "name": "silt", "version": "1.0" }
  match toml.pretty(data) {
    Ok(s) -> println(s)
    Err(e) -> println("Error: {e.message()}")
  }
}
```


## `toml.stringify`

```
toml.stringify(value: a) -> Result(String, TomlError)
```

Serialises a silt record or map to a TOML document. Like `toml.pretty`, the
top-level value must be table-shaped. A map is written as a table; a key of
it that is no `String` is written as its shown text (so the `Float` key `1.0`
reads `"1.0"`).

```silt
import toml

type Package {
  name: String,
  version: String,
}

fn main() {
  let data = Package { name: "silt", version: "1.0" }
  match toml.stringify(data) {
    Ok(s) -> println(s)
    Err(e) -> println("Error: {e.message()}")
  }
}
```
