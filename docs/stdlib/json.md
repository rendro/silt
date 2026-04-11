---
title: "json"
section: "Standard Library"
order: 10
---

# json

Parse JSON strings into typed silt values and serialize values to JSON.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `parse` | `(Type, String) -> Result(T, String)` | Parse JSON object into record |
| `parse_list` | `(Type, String) -> Result(List(T), String)` | Parse JSON array into record list |
| `parse_map` | `(Type, String) -> Result(Map(String, v), String)` | Parse JSON object into map |
| `pretty` | `(a) -> String` | Pretty-print value as JSON |
| `stringify` | `(a) -> String` | Serialize value as compact JSON |


## `json.parse`

```
json.parse(T: Type, s: String) -> Result(T, String)
```

Parses a JSON string into a record of type `T`. The first argument is a record
type name (not a string). Fields are matched by name; `Option` fields default to
`None` if missing from the JSON.

Fields of type `Date`, `Time`, and `DateTime` (from the `time` module) are
automatically parsed from ISO 8601 strings. `DateTime` fields also accept
timezone-aware formats (RFC 3339) — the offset is applied and the value is
stored as UTC:

| Field type | Accepted formats | Example |
|------------|-----------------|---------|
| `Date` | `YYYY-MM-DD` | `"2024-03-15"` |
| `Time` | `HH:MM:SS`, `HH:MM` | `"14:30:00"` |
| `DateTime` | `YYYY-MM-DDTHH:MM:SS`, with optional `Z` or `±HH:MM` offset | `"2024-03-15T09:00:00+09:00"` |

```silt
import json
type User {
    name: String,
    age: Int,
}

fn main() {
    let input = """{"name": "Alice", "age": 30}"""
    match json.parse(User, input) {
        Ok(user) -> println(user.name)
        Err(e) -> println("Error: {e}")
    }
}
```

Date/Time example:

```silt
import json
import time

type Event {
    name: String,
    date: Date,
}

fn main() -> Result(Unit, String) {
    let e = json.parse(Event, """{"name": "launch", "date": "2024-03-15"}""")?
    println(e.date |> time.weekday)  -- Friday
    Ok(())
}
```


## `json.parse_list`

```
json.parse_list(T: Type, s: String) -> Result(List(T), String)
```

Parses a JSON array where each element is a record of type `T`.

```silt
import json
import list
type Point {
    x: Int,
    y: Int,
}

fn main() {
    let input = """[{"x": 1, "y": 2}, {"x": 3, "y": 4}]"""
    match json.parse_list(Point, input) {
        Ok(points) -> list.each(points) { p -> println("{p.x}, {p.y}") }
        Err(e) -> println("Error: {e}")
    }
}
```


## `json.parse_map`

```
json.parse_map(V: Type, s: String) -> Result(Map(String, V), String)
```

Parses a JSON object into a `Map(String, V)`. The first argument is a type
descriptor (`Int`, `Float`, `String`, `Bool`, or a record type).

```silt
import json
import map
fn main() {
    let input = """{"x": 10, "y": 20}"""
    match json.parse_map(Int, input) {
        Ok(m) -> println(map.get(m, "x"))  -- Some(10)
        Err(e) -> println("Error: {e}")
    }
}
```


## `json.pretty`

```
json.pretty(value: a) -> String
```

Serializes any value to a pretty-printed JSON string (with indentation and
newlines).

```silt
import json
fn main() {
    let data = #{"name": "silt", "version": "1.0"}
    println(json.pretty(data))
}
```


## `json.stringify`

```
json.stringify(value: a) -> String
```

Serializes any value to a compact JSON string.

```silt
import json
fn main() {
    let data = #{"key": [1, 2, 3]}
    println(json.stringify(data))
    -- {"key":[1,2,3]}
}
```
