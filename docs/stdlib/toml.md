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
| `parse` | `(String, type a) -> Result(a, TomlError)` | Parse a TOML document (top-level table) into a record or a variant with fields |
| `parse_list` | `(String, type a) -> Result(List(a), TomlError)` | Parse the one array of a document (such as a `[[items]]` section) into a list |
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
| `TypeMismatch(expected, actual)` | `String`, `String` | A value's TOML type did not match the type it decodes into; `actual` ends with the value's path |
| `MissingField(name)` | `String` | A required (non-`Option`) field was absent; `name` is its path |
| `Other(msg)` | `String` | Anything else (serialization failures, document-shape violations) |

An error about a value of the document ends with the path of that value:
the fields of a record and the keys of a map by name (`inner.a`), a key
that is not made of ASCII letters, digits, `_` and `-` as a JSON string in
brackets (`tags["a b"]`), and the elements of a list or a tuple by index
(`xs[1]`; the list that `toml.parse_list` reads is under its one key, so
its elements are `points[2]`).
`TypeMismatch("Int", "string at inner.a")` reads `toml type mismatch: expected
Int, got string at inner.a`; a missing field is `MissingField("inner.a")`;
an `Other` about a value (a tuple of the wrong length, a date that is none)
ends with ` at <path>`. An error about the document itself, such as a
`Syntax` error or a document `toml.parse_list` finds no one array in, has
no path.

See [stdlib errors](errors.md) for the shared `Error` trait.


## Decodable types

The type argument of `toml.parse`, `toml.parse_list` and `toml.parse_map`
names the type to decode into:

| Function | Type argument |
|----------|---------------|
| `toml.parse` | a record type, an enum type, or a type with a `json.Decode` impl |
| `toml.parse_list` | `Int`, `Float`, `String`, `Bool`, a record type, an enum type, or a type with a `json.Decode` impl |
| `toml.parse_map` | `Int`, `Float`, `String`, `Bool`, a record type, an enum type, or a type with a `json.Decode` impl |

An alias of one of these is the type it names.

The fields of a record type, and of each variant of an enum type (and of every
record and enum type nested in them), may have these types:

- `Int`, `Float`, `String`, `Bool`
- `time.Date`, `time.Time`, `time.DateTime`
- `List(T)`, `Option(T)`, `Map(String, T)`
- tuples, such as `(Int, String)`
- non-generic record types
- non-generic enum types (see [Enums](#enums))
- type aliases of any of these

where `T` is again one of these types. Anything else (a `Set`, a generic
record or enum, a builtin enum such as `time.Weekday`, a function, a `Map`
whose key is not `String`, a type parameter) has no decoder.

A TOML integer goes into an `Int`, and into a `Float` where one is
expected. A TOML float goes into a `Float` only: `x = 1.0` for an `Int`
field is `Err(TypeMismatch("Int", "float at x"))`, because TOML says
which kind a number is. In the same way a datetime literal (`1990-01-01`,
`07:32:00`) goes into a `Date`, `Time` or `DateTime` field only: for a
`String` field `s` it is `Err(TypeMismatch("String", "datetime at s"))`, and the
text in quotes (`"1990-01-01"`) is a string. A value that is neither a
datetime literal nor a string, for a `Date` field `d`, is
`Err(TypeMismatch("time.Date", "integer at d"))` (`time.Time`, `time.DateTime` likewise).

The type a decoder is given is checked where it is known, as for
`json.parse`: written at the call (`toml.parse(text, Config)`), held in a
variable, given to a decoder used as a value, or given to a function or
method that passes it on to the decoder (each call of it is checked, a
trait method's on a value whose type is a type variable too). A
record with a field no decoder exists for is a compile error that names the
field and its type (or the field of a record type it names that has none);
so is an enum with a variant whose field has none (the error names the
variant), or a type the function does not take (such as
`toml.parse(text, Int)`, or `toml.parse(text, List(Int))`, whose error points to
`toml.parse_list`). The check applies equally to a
decoder imported by name (`import toml.{ parse }`) and to one called through
a module alias, and a type is refused whatever a document would hold (an
`Option` field whose key is left out, an empty list or map too).

A record or enum type with a written [`json.Decode`](json.md#jsondecode)
impl is decoded by the impl, as `json.parse` decodes it: one data layer,
one override. See [Types with a `json.Decode` impl](#types-with-a-jsondecode-impl).


## Types with a `json.Decode` impl

A type with a written `trait json.Decode for T` impl is decoded by its
`decode` wherever a value of the type is in the document: at the top, as an
element of `toml.parse_list`, as a value of `toml.parse_map`, in a record's
or a variant's field, in a list or a map in one. The impl gets its part of
the document as a `json.Value`: a table is a `json.Object`, an array a
`json.Array`, an integer a `json.Int`, a float a `json.Float`, and a date or
time literal a `json.String` of its text as TOML writes it, offset kept
(`1979-05-27`, `07:32:00.25`, `1979-05-27T07:32:00`,
`1979-05-27T07:32:00Z`, `1979-05-27T00:32:00-07:00`), which `json.decode`
reads back as a `Date`, `Time` or `DateTime`. A float that is no number
(`nan`, `inf`) has no `json.Value`: in such a part it is
`Err(TypeMismatch("json.Value", "a non-finite float at readings[1]"))`.

The fields of `T` are not looked into, so `T` may hold a type no decoder
has (a `Set`). An `Option` around it stays structural (a key left out is
`None`). The `JsonError` the impl returns is the `TomlError` of the same
variant, with the path of its part appended as to the decoder's own
errors: `Other("bad tags")` at `posts[1].tags` is `Other("bad tags at
posts[1].tags")`, `MissingField("name")` there is
`MissingField("posts[1].tags.name")`; a message is the impl's text with
the path appended as it is (`TypeMismatch("Int", "string at k at
posts[1].tags")` for one the impl got from `json.decode`). A `Syntax` error (an impl that
parses a JSON text of its own) is an `Other` of json's message of it,
`Other("json syntax error at byte 3: ... at posts[1].tags")`, since its
offset is in that text and not in the TOML document. The first error the
decoder comes to counts, the impl's or the decoder's own.


## Enums

An enum type the program declares, without type parameters, is decoded from
what `toml.stringify` writes of a variant, as `json.parse` decodes one (see
[json](json.md#enums)): a variant without fields is the string of its name,
and a variant with fields is a table whose `variant` names it and whose
`fields` array lists its fields.

```toml
status = "Active"
history = ["Active", { fields = [2], variant = "Paused" }]

[next]
fields = [1]
variant = "Paused"
```

An enum type may be the type argument (at the top of `toml.parse` only a
variant with fields is a table), the elements of `toml.parse_list`, the
values of `toml.parse_map`, and the type of a field anywhere in a record or
in another enum's variant. A document that is no variant of the enum is an
`Err` with the texts of `json.parse`, toml's names of the kinds of values and
the path: `status = "Gone"` is ``Other("`Status` has no variant `Gone` at
status")``, `status = 1` is `TypeMismatch("String or table", "integer at
status")`, and `status = { variant = "Active" }` is `TypeMismatch("String",
"table at status")`.


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

Parses a TOML document into a record, or a variant with fields, of type `a`
(see [Enums](#enums)). The type is passed as a
`type` parameter (see [Generics](../language/generics.md) — not a string).
Fields are matched by name; `Option` fields default to `None` if missing
from the document.

Fields of type `Date`, `Time`, and `DateTime` (from the `time` module) accept
either a TOML native datetime literal or an ISO 8601 string — the same formats
that `json.parse` accepts for each of those field types:

| Field type | Accepted formats | Example |
|------------|-----------------|---------|
| `Date` | `YYYY-MM-DD`, a year of more digits or with a sign as silt shows it (`12026-01-01`, `-001-01-01`) | `1979-05-27` or `"1979-05-27"` |
| `Time` | `HH:MM:SS`, `HH:MM`, `HH:MM:SS.fffffffff` | `07:32:00` or `"07:32:00.5"` |
| `DateTime` | RFC 3339 / ISO 8601 (`YYYY-MM-DDTHH:MM:SS`, `YYYY-MM-DDTHH:MM`, a fractional second), with optional `Z` or `±HH:MM` offset; the year as for `Date` | `1979-05-27T07:32:00Z` |

A fractional second is read to the nanosecond; further digits are cut. A
second 60, a leap second, is no time silt holds: it is refused with an `Other`
that names the text, from a string and from a literal alike:
`invalid time '23:59:60' (expected HH:MM:SS): a leap second (second 60) is not read`.
A date or a time that is none has one message whether it is written in
quotes or as a literal: `d = 07:32:00` for a `Date` field `d` is
`invalid date '07:32:00' (expected YYYY-MM-DD): input contains invalid characters at d`.

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
accepts a document whose top-level table contains exactly one key, an array,
and returns the list of values under that key, each decoded into `a`. The type
is passed as a `type` parameter (`Int`, `Float`, `String`, `Bool`, a record
type, an enum type, or a type with a `json.Decode` impl), as for
`toml.parse_map`. For a record type the array is the natural
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

The key name (`points` in the example above) is not checked: any single
top-level key works, `xs = [1, 2, 3]` for `toml.parse_list(text, Int)` too.


## `toml.parse_map`

```
toml.parse_map(s: String, type a) -> Result(Map(String, a), TomlError)
```

Parses a top-level TOML table into a `Map(String, v)`. The type is passed as
a `type` parameter (`Int`, `Float`, `String`, `Bool`, a record type, or an
enum type).

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
`[section]` headers, arrays of tables as `[[section]]` blocks), so
`toml.pretty` differs from `toml.stringify` only in writing an array of plain
values one element per line:

```text
p = [
    1,
    2,
]
```

Note: the top-level value must be a record, a map or a variant with fields;
TOML cannot represent a bare scalar or array at top level (nor a variant
without fields, which is a string).

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

Serialises a silt record, map or variant with fields to a TOML document. Like `toml.pretty`, the
top-level value must be table-shaped. Each kind of value is written as
follows:

| Value | Written as |
|-------|------------|
| `Int`, `Float`, `String`, `Bool` | an integer, a float, a string, `true` or `false` |
| `List(a)`, a tuple | an array: `(1, "a")` is `[1, "a"]` |
| `Map(k, v)` | a table; a key that is no `String` is written as its shown text (the `Float` key `1.0` reads `"1.0"`); a key whose value is `None` is left out |
| a record | a table of its fields, in the order the type declares them; a field that is `None` is left out (TOML has no null), and reads back as `None` |
| `Some(x)` | `x`, as `x` is written |
| `None` in an array (a list, a tuple, the fields of a variant) | nothing: `Err(Other("toml.stringify: TOML has no null: a None in an array"))` |
| a `json.Value` | the TOML value it holds: `json.Int(1)` as `1`, `json.Array` as an array, `json.Object` as a table (its keys sorted); `json.Null` as `None` is |
| a value of a type with a written [`json.Encode`](json.md#jsonencode) impl | the `json.Value` its `encode` gives, as a `json.Value` is written, wherever the value is |
| any other variant (of `Result` too) | a table with the keys `fields` (an array) and `variant` (its name); a variant without fields is the string of its name |
| `Date`, `Time`, `DateTime` | TOML's own literals: `2026-10-19`, `12:30:00` (with a fractional second only when the value has one: `12:30:00.25`) and `2026-10-19T12:30:00` |
| `Duration`, `Instant` | as the records they are: a table with `ns` or `epoch_ns` |
| `()` | nothing: `Err(Other("toml.stringify: TOML cannot represent Unit"))` |

The keys of a map are written in sorted order, the fields of a record in the
order its type declares them; either way the plain keys of a table come
before its sub-tables (`[name]`, `[[name]]`), as TOML requires. `toml.parse`
reads a `Date`, a `Time` and a `DateTime` back into a field of the same type.
A date whose year has no literal (one before the year 0 or past 9999) is
written as a string of the text silt shows of it, which `toml.parse` reads
back as well. A variant is read back into its enum type when the program
declares it (see [Enums](#enums)); a variant of a builtin enum is written but
not read back.

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
