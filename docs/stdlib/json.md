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
| `encode` | `(a) -> Value where a: Encode` | Encode a value as a json.Value |
| `parse` | `(String, type a) -> Result(a, JsonError)` | Parse JSON object into record |
| `parse_list` | `(String, type a) -> Result(List(a), JsonError)` | Parse a JSON array into a list |
| `parse_map` | `(String, type a) -> Result(Map(String, a), JsonError)` | Parse JSON object into map |
| `pretty` | `(a) -> String where a: Encode` | Pretty-print value as JSON |
| `stringify` | `(a) -> String where a: Encode` | Serialize value as compact JSON |

## Errors

Every fallible `json.*` call returns `Result(T, JsonError)`. The `JsonError`
enum has four variants you can pattern-match on, or fall back to
`e.message()` when you just want a rendered string (`trait Error for
JsonError` is wired in):

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Syntax(msg, offset)` | `String`, `Int` | Malformed JSON at `offset` bytes, or a number beyond `Float`'s range (`1e400`: `number out of range`) |
| `TypeMismatch(expected, actual)` | `String`, `String` | A value's JSON type did not match the type it decodes into; `actual` ends with the value's path |
| `MissingField(name)` | `String` | A required (non-`Option`) field was absent; `name` is its path |
| `Other(msg)` | `String` | Anything else: a tuple's array of the wrong length, a value too deep for `json.decode` |

An error about a value of the document ends with the path of that value:
the fields of a record and the keys of a map by name (`inner.a`), a key
that is not made of ASCII letters, digits, `_` and `-` as a JSON string in
brackets (`tags["a b"]`), and the elements of a list or a tuple by index
(`xs[1]`; `[2]` for an element of the list that `json.parse_list`
reads).
`TypeMismatch("Int", "string at inner.a")` reads `json type mismatch: expected
Int, got string at inner.a`; a missing field is `MissingField("inner.a")`;
an `Other` about a value (a tuple of the wrong length, a date that is none)
ends with ` at <path>`. An error about the document itself, such as a
`Syntax` error or an array where a record is expected, has no path.

See [stdlib errors](errors.md) for the shared `Error` trait.


## Decodable types

The type argument of `json.parse`, `json.parse_list`, `json.parse_map` and
`json.decode` names the type to decode into:

| Function | Type argument |
|----------|---------------|
| `json.parse` | `Int`, `Float`, `String`, `Bool`, `json.Value`, a record type, or an enum type |
| `json.parse_list` | `Int`, `Float`, `String`, `Bool`, `json.Value`, a record type, or an enum type |
| `json.parse_map` | `Int`, `Float`, `String`, `Bool`, `json.Value`, a record type, or an enum type |
| `json.decode` | `Int`, `Float`, `String`, `Bool`, `json.Value`, a record type, or an enum type |

An alias of one of these is the type it names (`type Member = User`, then
`json.parse(text, Member)`).

The fields of a record type, and of each variant of an enum type (and of every
record and enum type nested in them), may have these types:

- `Int`, `Float`, `String`, `Bool`
- `Date`, `Time`, `DateTime`
- `json.Value`, which keeps that part of the document as it is
- `List(T)`, `Option(T)`, `Map(String, T)`
- tuples, such as `(Int, String)`
- non-generic record types
- non-generic enum types (see [Enums](#enums))
- type aliases of any of these

where `T` is again one of these types. Anything else (a `Set`, a generic
record or enum, a builtin enum such as `time.Weekday`, a function, a `Map`
whose key is not `String`, a type parameter) has no decoder.

A number of the document goes into an `Int` when it is a whole number in
`Int`'s range (`1`, `1.0`, `1e3`). A number with a fraction (`1.5`), or one out of
that range, is `Err(TypeMismatch("Int", ...))`: nothing is cut off or rounded.

The type a decoder is given is checked where it is known: written at the
call (`json.parse(text, Config)`), held in a variable, given to a decoder
used as a value, or given to a function or method that passes it on to the
decoder (`fn load(text, type a) { json.parse(text, a) }`: each call of
`load` is checked, and the error notes that `load` passes it to
`json.parse`). A trait method passes it on too, also one called on a value
whose type is a type variable (`fn go(l: l, type b) where l: Loader {
l.load(b) }`): each call of `go` is checked against the method of the type
it gives `l`. A
record with a field no decoder exists for is a compile error that names the
field and its type, and so is an enum with a variant whose field has none
(the error names the variant); so is a generic record or enum, or a type the
function does not take (such as `json.parse(text, List)`). The check applies
equally to a decoder imported by name (`import json.{ parse }`) and to one
called through a module alias, and a type is refused whatever a document
would hold (an `Option` field whose key is left out, another variant of the
enum, an empty list or map too).


## Enums

An enum type the program declares, without type parameters, is decoded from
what `json.stringify` writes of a variant, so a variant written with
`json.stringify` reads back as the same variant:

| Variant | JSON |
|---------|------|
| without fields, `Active` | `"Active"` |
| with fields, `Moved("x", Some(2))` | `{"fields":["x",2],"variant":"Moved"}` |

A variant without fields is the string of its name, wherever it is (in a
record, a list, a map, the fields of another variant). Of a variant with
fields, `"variant"` names the variant and `"fields"` lists its fields in the
order the variant declares them, each decoded as a record field of its type is
(a record, a list, an option, another enum); other keys of the object are
ignored.

An enum type may be decoded at the top (`json.parse(text, Status)`), as the
elements of `json.parse_list` and the values of `json.parse_map`, and as the
type of a field anywhere in a record or in another enum's variant. A document
that is no variant of the enum is an `Err`, which ends with the path, as
the error of a record does (here of an enum at the top):

| Document | Error |
|----------|-------|
| a string the enum has no variant of | ``Other("`Status` has no variant `Gone`")`` |
| the name of a variant with fields | ``Other("variant `Paused` of `Status` has 1 field, got 0")`` |
| no string and no object (a number, an array) | `TypeMismatch("String or object", "number")` |
| an object that names a variant without fields | `TypeMismatch("String", "object")` |
| an object without a `"variant"` key | `MissingField("variant")` |
| a `"variant"` that is no string | `TypeMismatch("String", "number at variant")` |
| a `"variant"` the enum has no variant of | ``Other("`Status` has no variant `Gone` at variant")`` |
| `"fields"` that is no array | `TypeMismatch("array", "number at fields")` |
| the wrong number of fields | ``Other("variant `Paused` of `Status` has 1 field, got 2 at fields")`` |
| a field of the wrong type | the error of a value of that type, such as `TypeMismatch("Int", "string at fields[0]")` |

```silt
import json

type Status {
  Active,
  Paused(Int),
}

type Job {
  name: String,
  status: Status,
}

fn main() {
  let job = Job { name: "build", status: Paused(3) }
  let text = json.stringify(job)
  println(text) -- {"name":"build","status":{"fields":[3],"variant":"Paused"}}
  println(json.parse(text, Job)) -- Ok(Job {name: build, status: Paused(3)})
  println(json.stringify([Active, Paused(1)])) -- ["Active",{"fields":[1],"variant":"Paused"}]
  println(json.parse("""{"name": "x", "status": "Gone"}""", Job))
  -- Err(`Status` has no variant `Gone` at status)
}
```

The toml decoders read an enum in the same forms (see
[toml](toml.md#enums)).


## Nesting limits

`json.stringify` and `json.pretty` write a value nested up to 1,000 levels
deep. The element of a list or of a tuple, the field of a record or of an
enum variant and the value of a map are each one level further in than what
holds them; the content of a `Some` is not (it is written in the `Some`'s
place). A value nested deeper is a runtime error:

```text
json.stringify: the value is nested more than 1000 levels deep
```

The parsers stop sooner: `json.parse`, `json.parse_list` and
`json.parse_map` read a document whose arrays and objects are nested up to
127 levels deep, and give a `Syntax` error (`recursion limit exceeded`) for
one nested deeper. A value written at more levels than that is valid JSON
that silt itself does not read back.

`json.decode` reads a `json.Value` nested up to 1,000 levels deep, counted
as for `json.stringify`; one nested deeper is an `Err(Other(msg))`:

```text
json.decode: the value is nested more than 1000 levels deep
```


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


## `json.Encode`

```
trait Encode {
  fn encode(self) -> Value
}
```

What `json.stringify` and `json.pretty` need of the value they write. A
type has it by its structure, as it has `Display`: `Int`, `Float`, `String`,
`Bool`, `()`, a `json.Value`, a `Date`, a `Time` and a `DateTime` have it; a
list, a tuple, an `Option` and a record or enum type (the program's or a
builtin module's) have it when their parts do; a map when its values do (a
key is written as its text). `Bytes`, a set, a function, a channel and a
handle have none. A function that hands a value of a type variable to
`json.stringify` says so, as for `Display`; one without annotations is
inferred to need it:

```silt
import json

fn send(x: a) -> String where a: json.Encode {
  json.stringify(x)
}

fn main() {
  println(send([1, 2])) -- [1,2]
}
```

Without the `where` clause the definition is an error:

```text
type variable `a` is not known to implement trait 'json.Encode', which 'json.stringify' needs
```

A program names the trait with its module, `json.Encode`.

Write `trait json.Encode for T` to replace the structural form of `T`, in
the module that declares `T` (as a `Display` impl is written; an impl for
a builtin type is an error, a builtin type keeps its own form). Its
`encode` gives the `json.Value` the type is written as, wherever a value of
the type is inside what is written: at the top, in a list, as a map's
value, in a record's or a variant's field. A `Some` around it stays
structural (`Some(x)` is written as `x` is), and a type that holds a
function may have a written impl. An impl for a generic type is allowed;
what it asks of the type's arguments it says in a `where` clause.

```silt
import json

type Money {
  cents: Int,
}

trait json.Encode for Money {
  fn encode(self) -> json.Value {
    json.Int(self.cents)
  }
}

type Order {
  id: Int,
  total: Money,
}

fn main() {
  println(json.stringify(Order { id: 7, total: Money { cents: 1250 } }))
  -- {"id":7,"total":1250}, not {"id":7,"total":{"cents":1250}}
}
```


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


## `json.encode`

```
json.encode(value: a) -> Value where a: Encode
```

The `json.Value` of what `json.stringify` writes of a value: what
`json.parse` reads of that text. A whole number is a `json.Int` (`1.0` too),
a record and a map are a `json.Object`, whose keys come out sorted (so
`json.stringify(json.encode(r))` writes a record's fields in the order of
their names, `json.stringify(r)` in the order its type declares them), a
`None` and `()` are `json.Null`. Written [`json.Encode`](#jsonencode) impls
are used wherever their types are in the value, at the top too: the
counterpart of [`json.decode`](#jsondecode). An impl's `encode` uses it for
the parts it writes; for its own value (`json.encode(self)`) it would call
itself without end. In a module that imports json, `x.encode()` is the
method of the trait: on a value whose type has a written impl it is what
the impl gives, as it gives it (`json.Float(1.0)` stays a `Float`); on any
other value it is `json.encode(x)`.

```silt
import json

type Pair {
  left: Int,
  right: Int,
}

trait json.Encode for Pair {
  fn encode(self) -> json.Value {
    json.Array([json.encode(self.left), json.encode(self.right)])
  }
}

fn main() {
  println(json.encode([1, 2])) -- Array([Int(1), Int(2)])
  println(json.stringify(Pair { left: 1, right: 2 })) -- [1,2]
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

Parses a JSON array into a list of `a`, each element decoded as
`json.parse` decodes a document into `a`. The type is passed as a `type`
parameter (`Int`, `Float`, `String`, `Bool`, `json.Value`, a record type, or
an enum type, see [Enums](#enums)), as for `json.parse_map`.

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
parameter (`Int`, `Float`, `String`, `Bool`, `json.Value`, a record type, or
an enum type).

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
json.pretty(value: a) -> String where a: Encode
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
json.stringify(value: a) -> String where a: Encode
```

Serializes a value to a compact JSON string. Each kind of value is
written as follows:

| Value | Written as |
|-------|------------|
| `Int`, `Float`, `String`, `Bool` | a number, a string, `true` or `false` |
| `List(a)`, a tuple | an array: `(1, "a")` is `[1,"a"]` |
| `Map(k, v)` | an object; a key that is no `String` is written as its shown text (the `Float` key `1.0` reads `"1.0"`) |
| a record | an object of its fields, in the order the type declares them: `{"name":"Ada","age":36}` |
| `Some(x)` | `x`, as `x` is written |
| `None`, `()` | `null` |
| any other variant (of `Result` too) | `{"fields":[...],"variant":"Name"}`; a variant without fields is the string `"Name"` |
| `json.Value` | the JSON it holds: `json.Int(1)` is `1`, `json.Null` is `null` |
| `Date`, `Time`, `DateTime` | the text silt shows of them: `"2026-10-19"`, `"12:30:00"` (with a fractional second only when the value has one: `"12:30:00.250000000"`) and `"2026-10-19T12:30:00"` |
| `Duration`, `Instant` | as the records they are: `{"ns":2000000000}`, `{"epoch_ns":...}` |

The keys of a map, and so of a `json.Object`, are written in sorted order;
the fields of a record in the order its type declares them. `json.parse` reads a `Date`, a `Time` and a `DateTime` back into a field of
the same type, a record into its record type, and a variant into its enum
type when the program declares it (see [Enums](#enums)); a variant of a
builtin enum and `()` are written but not read back.

`json.stringify` and `json.pretty` write the same values: those of a type
that has [`json.Encode`](#jsonencode). A type with no json form is a compile
error that names the way down to the part that has none, wherever in the
value it is: `Bytes`, a `Set`, a function (the constructor of a variant,
such as `Some`, is one), a channel, a task handle, a TCP listener or stream.

```text
type 'List(Reply)' does not implement trait 'json.Encode': element > field 'body' is of type 'Bytes', which does not
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
