---
title: "map"
section: "Standard Library"
order: 4
---

# map

Functions for working with immutable, ordered maps (`Map(k, v)`). Maps use
`#{key: value}` literal syntax. Keys must satisfy the `Hash` trait constraint.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `contains` | `(Map(k, v), k) -> Bool` | Check if key exists |
| `delete` | `(Map(k, v), k) -> Map(k, v)` | Remove a key |
| `each` | `(Map(k, v), (k, v) -> ()) -> ()` | Iterate over all entries |
| `entries` | `(Map(k, v)) -> List((k, v))` | All key-value pairs as tuples |
| `filter` | `(Map(k, v), (k, v) -> Bool) -> Map(k, v)` | Keep entries matching predicate |
| `from_entries` | `(List((k, v))) -> Map(k, v)` | Build map from tuple list |
| `get` | `(Map(k, v), k) -> Option(v)` | Look up value by key |
| `keys` | `(Map(k, v)) -> List(k)` | All keys as a list |
| `length` | `(Map(k, v)) -> Int` | Number of entries |
| `map` | `(Map(k, v), (k, v) -> (k2, v2)) -> Map(k2, v2)` | Transform all entries |
| `merge` | `(Map(k, v), Map(k, v)) -> Map(k, v)` | Merge two maps (right wins) |
| `set` | `(Map(k, v), k, v) -> Map(k, v)` | Insert or update a key |
| `update` | `(Map(k, v), k, v, (v) -> v) -> Map(k, v)` | Update existing or insert default |
| `values` | `(Map(k, v)) -> List(v)` | All values as a list |


## `map.contains`

```
map.contains(m: Map(k, v), key: k) -> Bool
```

Returns `true` if the map has an entry for `key`.

```silt
import map
fn main() {
    let m = #{"a": 1, "b": 2}
    println(map.contains(m, "a"))  -- true
    println(map.contains(m, "z"))  -- false
}
```


## `map.delete`

```
map.delete(m: Map(k, v), key: k) -> Map(k, v)
```

Returns a new map with `key` removed. No-op if key does not exist.

```silt
import map
fn main() {
    let m = #{"a": 1, "b": 2}
    let m2 = map.delete(m, "a")
    println(map.length(m2))  -- 1
}
```


## `map.each`

```
map.each(m: Map(k, v), f: (k, v) -> ()) -> ()
```

Calls `f` with each key-value pair. Used for side effects.

```silt
import map
fn main() {
    let m = #{"x": 10, "y": 20}
    map.each(m) { k, v -> println("{k} = {v}") }
}
```


## `map.entries`

```
map.entries(m: Map(k, v)) -> List((k, v))
```

Returns all key-value pairs as a list of tuples.

```silt
import map
fn main() {
    let m = #{"a": 1, "b": 2}
    let pairs = map.entries(m)
    -- [("a", 1), ("b", 2)]
}
```


## `map.filter`

```
map.filter(m: Map(k, v), f: (k, v) -> Bool) -> Map(k, v)
```

Returns a new map containing only entries where `f` returns `true`.

```silt
import map
fn main() {
    let m = #{"a": 1, "b": 2, "c": 3}
    let big = map.filter(m) { k, v -> v > 1 }
    -- #{"b": 2, "c": 3}
}
```


## `map.from_entries`

```
map.from_entries(entries: List((k, v))) -> Map(k, v)
```

Builds a map from a list of `(key, value)` tuples. Later entries overwrite
earlier ones with the same key.

```silt
import map
fn main() {
    let m = map.from_entries([("a", 1), ("b", 2)])
    println(m)  -- #{"a": 1, "b": 2}
}
```


## `map.get`

```
map.get(m: Map(k, v), key: k) -> Option(v)
```

Returns `Some(value)` if the key exists, or `None` otherwise.

```silt
import map
fn main() {
    let m = #{"name": "silt"}
    match map.get(m, "name") {
        Some(v) -> println(v)
        None -> println("not found")
    }
}
```


## `map.keys`

```
map.keys(m: Map(k, v)) -> List(k)
```

Returns all keys as a list, in sorted order.

```silt
import map
fn main() {
    let ks = map.keys(#{"b": 2, "a": 1})
    println(ks)  -- ["a", "b"]
}
```


## `map.length`

```
map.length(m: Map(k, v)) -> Int
```

Returns the number of entries in the map.

```silt
import map
fn main() {
    println(map.length(#{"a": 1, "b": 2}))  -- 2
}
```


## `map.map`

```
map.map(m: Map(k, v), f: (k, v) -> (k2, v2)) -> Map(k2, v2)
```

Transforms each entry. The callback must return a `(key, value)` tuple.

```silt
import map
fn main() {
    let m = #{"a": 1, "b": 2}
    let doubled = map.map(m) { k, v -> (k, v * 2) }
    -- #{"a": 2, "b": 4}
}
```


## `map.merge`

```
map.merge(m1: Map(k, v), m2: Map(k, v)) -> Map(k, v)
```

Merges two maps. When both have the same key, the value from `m2` wins.

```silt
import map
fn main() {
    let a = #{"x": 1, "y": 2}
    let b = #{"y": 99, "z": 3}
    let merged = map.merge(a, b)
    -- #{"x": 1, "y": 99, "z": 3}
}
```


## `map.set`

```
map.set(m: Map(k, v), key: k, value: v) -> Map(k, v)
```

Returns a new map with the key set to value. Inserts if new, overwrites if
existing.

```silt
import map
fn main() {
    let m = #{"a": 1}
    let m2 = map.set(m, "b", 2)
    println(m2)  -- #{"a": 1, "b": 2}
}
```


## `map.update`

```
map.update(m: Map(k, v), key: k, default: v, f: (v) -> v) -> Map(k, v)
```

If `key` exists, applies `f` to the current value. If `key` does not exist,
applies `f` to `default`. Inserts the result.

```silt
import map
fn main() {
    let m = #{"a": 1}
    let m2 = map.update(m, "a", 0) { v -> v + 10 }
    let m3 = map.update(m2, "b", 0) { v -> v + 10 }
    -- m2 == #{"a": 11}
    -- m3 == #{"a": 11, "b": 10}
}
```


## `map.values`

```
map.values(m: Map(k, v)) -> List(v)
```

Returns all values as a list, in key-sorted order.

```silt
import map
fn main() {
    let vs = map.values(#{"a": 1, "b": 2})
    println(vs)  -- [1, 2]
}
```
