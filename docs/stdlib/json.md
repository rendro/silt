---
title: "json"
section: "Standard Library"
order: 10
---

# json

Parse JSON strings into typed silt values and serialize values to JSON.
A document whose shape is not known in advance is read into a
[`json.Value`](#jsonvalue).

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `decode` | `(Value, type a) -> Result(a, JsonError)` | Decode a json.Value into a typed value |
| `parse` | `(String, type a) -> Result(a, JsonError)` | Parse JSON object into record |
| `parse_list` | `(String, type a) -> Result(List(a), JsonError)` | Parse JSON array into record list |
| `parse_map` | `(String, type a) -> Result(Map(String, a), JsonError)` | Parse JSON object into map |
| `pretty` | `(a) -> String` | Pretty-print value as JSON |
| `stringify` | `(a) -> String` | Serialize value as compact JSON |

## Errors

Every fallible `json.*` call returns `Result(T, JsonError)`. The `JsonError`
enum has four variants you can pattern-match on, or fall back to
`e.message()` when you just want a rendered string (`trait Error for
JsonError` is wired in):

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Syntax(msg, offset)` | `String`, `Int` | Malformed JSON at `offset` bytes |
| `TypeMismatch(expected, actual)` | `String`, `String` | A field's JSON type did not match the target field type |
| `MissingField(name)` | `String` | A required (non-`Option`) field was absent |
| `Other(msg)` | `String` | Anything else (out-of-range numbers, internal failures) |

See [stdlib errors](errors.md) for the shared `Error` trait.


## Decodable types

The type argument of `json.parse`, `json.parse_list`, `json.parse_map` and
`json.decode` names the type to decode into:

| Function | Type argument |
|----------|---------------|
| `json.parse` | `Int`, `Float`, `String`, `Bool`, `json.Value`, or a record type |
| `json.parse_list` | `json.Value` or a record type |
| `json.parse_map` | `Int`, `Float`, `String`, `Bool`, `json.Value`, or a record type |
| `json.decode` | `Int`, `Float`, `String`, `Bool`, `json.Value`, or a record type |

An alias of one of these is the type it names (`type Member = User`, then
`json.parse(text, Member)`).

The fields of a record type (and of every record type nested in it) may have
these types:

- `Int`, `Float`, `String`, `Bool`
- `Date`, `Time`, `DateTime`
- `json.Value`, which keeps that part of the document as it is
- `List(T)`, `Option(T)`, `Map(String, T)`
- tuples, such as `(Int, String)`
- non-generic record types
- type aliases of any of these

where `T` is again one of these types. Anything else (a `Set`, an enum, a
generic record, a function, a `Map` whose key is not `String`, a type
parameter) has no decoder.

A number of the document goes into an `Int` when it is a whole number in
`Int`'s range (`1`, `1.0`, `1e3`). A number with a fraction (`1.5`), or one out of
that range, is `Err(TypeMismatch("Int", ...))`: nothing is cut off or rounded.

When the type is written at the call, `json.parse(text, Config)`, the
compiler checks it. A record with a field no decoder exists for is a compile
error that names the field and its type; so is an enum, or a type the function
does not take (such as `json.parse_list(text, Int)`). The check applies
equally to a decoder imported by name (`import json.{ parse }`) and to one
called through a module alias. When the type only arrives at run time, through
a `type a` parameter, the same problems are reported then: a field without a
decoder gives an `Err(Other(msg))` whose message names the field and its
record, and a type the function does not take is a run-time error.


## Nesting limits

`json.stringify` and `json.pretty` write a value nested up to 1,000 levels
deep. The element of a list or of a tuple, the field of a record or of an
enum variant and the value of a map are each one level further in than what
holds them; the content of a `Some` is not (it is written in the `Some`'s
place). A value nested deeper is a runtime error, as is any value they cannot
write:

```text
json.stringify: the value is nested more than 1000 levels deep
```

The parsers stop sooner: `json.parse`, `json.parse_list` and
`json.parse_map` read a document whose arrays and objects are nested up to
127 levels deep, and give a `Syntax` error (`recursion limit exceeded`) for
one nested deeper. A value written at more levels than that is valid JSON
that silt itself does not read back.


## `json.Value`

```
type Value {
  Null,
  Bool(Bool),
  Int(Int),
  Float(Float),
  String(String),
  Array(List(Value)),
  Object(Map(String, Value)),
}
```

A JSON document as it is, for input whose shape is not known in advance:
any document reads into it. Its variants are written through the module,
`json.Null`, `json.Int(n)`, `json.Object(fields)`. When the shape is known,
decode into a record type instead: a mistake in the document is then an
error where it is read.

- A number is an `Int` when it is a whole number in `Int`'s range (`1`,
  `1.0`, `1e3`), as for an `Int` field, and a `Float` otherwise (`1.5`,
  a number out of that range).
- An object is a `Map(String, Value)`. Its keys come out sorted, not in
  the document's order, and of a key written twice the last value counts.

`json.stringify` and `json.pretty` write a `json.Value` as the JSON it
holds: `json.Int(1)` as `1`, `json.Object` as an object.

```silt
import json
import map

fn name_of(body: String) -> Option(String) {
  match json.parse(body, json.Value) {
    Ok(json.Object(fields)) -> match map.get(fields, "name") {
      Some(json.String(name)) -> Some(name)
      _ -> None
    }
    _ -> None
  }
}

fn main() {
  println(name_of("""{"name": "Ada", "age": 36, "tags": ["x"]}""")) -- Some(Ada)
  let reply = json.Object(#{ "name": json.String("Ada"), "age": json.Int(36) })
  println(json.stringify(reply)) -- {"age":36,"name":"Ada"}
}
```

A record field of type `json.Value` keeps that part of the document as it
is, for [`json.decode`](#jsondecode) to read once its shape is known. Like
any field that is no `Option`, it must be in the document (a JSON `null`
is `json.Null`); a field of type `Option(json.Value)` may be left out.


## `json.decode`

```
json.decode(value: Value, type a) -> Result(a, JsonError)
```

Decodes a `json.Value` into the type `a`, as `json.parse` decodes the
same document as text, with the same errors.

```silt
import json

type Event {
  kind: String,
  data: json.Value,
}

type Resize {
  width: Int,
  height: Int,
}

fn describe(text: String) -> Result(String, json.JsonError) {
  let event = json.parse(text, Event)?
  match event.kind {
    "resize" -> {
      let size = json.decode(event.data, Resize)?
      Ok("resize to {size.width}x{size.height}")
    }
    _ -> Ok("ignored {event.kind}")
  }
}

fn main() {
  println(describe("""{"kind": "resize", "data": {"width": 3, "height": 4}}"""))
  -- Ok(resize to 3x4)
}
```


## `json.parse`

```
json.parse(s: String, type a) -> Result(a, JsonError)
```

Parses a JSON string into a record of type `a`. The type is passed as a `type`
parameter (see [Generics](../language/generics.md) — not a string). Fields are
matched by name; `Option` fields default to `None` if missing from the JSON.

Fields of type `Date`, `Time`, and `DateTime` (from the `time` module) are
automatically parsed from ISO 8601 strings. `DateTime` fields also accept
timezone-aware formats (RFC 3339) — the offset is applied and the value is
stored as UTC:

| Field type | Accepted formats | Example |
|------------|-----------------|---------|
| `Date` | `YYYY-MM-DD`, a year of more digits or with a sign as silt shows it (`12026-01-01`, `-001-01-01`) | `"2024-03-15"` |
| `Time` | `HH:MM:SS`, `HH:MM`, `HH:MM:SS.fffffffff` | `"14:30:00"`, `"14:30:00.25"` |
| `DateTime` | `YYYY-MM-DDTHH:MM:SS`, `YYYY-MM-DDTHH:MM`, a fractional second, with optional `Z` or `±HH:MM` offset; the year as for `Date` | `"2024-03-15T09:00:00+09:00"` |

A fractional second is read to the nanosecond; further digits are cut. A
second 60, a leap second, is no time silt holds: it is refused with an
`Other` that names the text, such as
`invalid time '23:59:60' (expected HH:MM:SS): a leap second (second 60) is not read`.

```silt
import json

type User {
  name: String,
  age: Int,
}

fn main() {
  let input = """{"name": "Alice", "age": 30}"""
  match json.parse(input, User) {
    Ok(user) -> println(user.name)
    Err(e) -> println("Error: {e.message()}")
  }
}
```

Date/Time example:

```silt
import json
import time

type Event {
  name: String,
  date: time.Date,
}

fn main() -> Result(Unit, json.JsonError) {
  let e = json.parse("""{"name": "launch", "date": "2024-03-15"}""", Event)?
  println(e.date |> time.weekday) -- Friday
  Ok(())
}
```


## `json.parse_list`

```
json.parse_list(s: String, type a) -> Result(List(a), JsonError)
```

Parses a JSON array where each element is a record of type `a`, or, for
`json.Value`, any value.

```silt
import json
import list

type Point {
  x: Int,
  y: Int,
}

fn main() {
  let input = """[{"x": 1, "y": 2}, {"x": 3, "y": 4}]"""
  match json.parse_list(input, Point) {
    Ok(points) -> list.each(points) { p -> println("{p.x}, {p.y}") }
    Err(e) -> println("Error: {e.message()}")
  }
}
```


## `json.parse_map`

```
json.parse_map(s: String, type a) -> Result(Map(String, a), JsonError)
```

Parses a JSON object into a `Map(String, v)`. The type is passed as a `type`
parameter (`Int`, `Float`, `String`, `Bool`, `json.Value`, or a record type).

```silt
import json
import map

fn main() {
  let input = """{"x": 10, "y": 20}"""
  match json.parse_map(input, Int) {
    Ok(m) -> println(map.get(m, "x")) -- Some(10)
    Err(e) -> println("Error: {e.message()}")
  }
}
```


## `json.pretty`

```
json.pretty(value: a) -> String
```

Serializes a value to a pretty-printed JSON string (with indentation and
newlines). It writes and refuses the same values as `json.stringify`.

```silt
import json

fn main() {
  let data = #{ "name": "silt", "version": "1.0" }
  println(json.pretty(data))
}
```


## `json.stringify`

```
json.stringify(value: a) -> String
```

Serializes a value to a compact JSON string. A map is written as an
object; a key of it that is no `String` is written as its shown text (so the
`Float` key `1.0` reads `"1.0"`). A `json.Value` is written as the JSON it
holds. A `Date`, a `Time` and a `DateTime` (from the `time` module) are
written as the text silt shows of them, which `json.parse` reads back into a
field of the same type: `"2026-10-19"`, `"12:30:00"` (with a fractional second
only when the value has one: `"12:30:00.250000000"`) and
`"2026-10-19T12:30:00"`.

`json.stringify` and `json.pretty` write the same values. A value JSON has no
form for is a runtime error that names its kind, wherever in the value it is:
`Bytes`, a `Set`, a function (the constructor of a variant, such as `Some`,
is one), a channel, a task handle, a TCP listener or stream.

```text
json.stringify: unsupported value kind Bytes
```

To send bytes as JSON, turn them into text first (`bytes.to_string`,
`bytes.to_hex` or `bytes.to_base64`); to send a set, make a list of it
(`set.to_list`).

```silt
import json
import time

fn main() {
  let data = #{ "key": [1, 2, 3] }
  println(json.stringify(data))
  -- {"key":[1,2,3]}
  println(json.stringify(#{ 1.0: "one" })) -- {"1.0":"one"}
  println(json.stringify(time.Date { year: 2026, month: 10, day: 19 }))
  -- "2026-10-19"
}
```
