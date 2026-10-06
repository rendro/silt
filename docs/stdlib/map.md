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
| `contains` | `(Map(a, b), a) -> Bool where a: Hash` | Check if key exists |
| `delete` | `(Map(a, b), a) -> Map(a, b) where a: Hash` | Remove a key |
| `each` | `(Map(a, b), Fn(a, b) -> ()) -> ()` | Iterate over all entries |
| `entries` | `(Map(a, b)) -> List((a, b))` | All key-value pairs as tuples |
| `filter` | `(Map(a, b), Fn(a, b) -> Bool) -> Map(a, b)` | Keep entries matching predicate |
| `from_entries` | `(List((a, b))) -> Map(a, b)` | Build map from tuple list |
| `get` | `(Map(a, b), a) -> Option(b) where a: Hash` | Look up value by key |
| `keys` | `(Map(a, b)) -> List(a)` | All keys as a list |
| `length` | `(Map(a, b)) -> Int` | Number of entries |
| `map` | `(Map(a, b), Fn(a, b) -> (c, d)) -> Map(c, d)` | Transform all entries |
| `merge` | `(Map(a, b), Map(a, b)) -> Map(a, b) where a: Hash` | Merge two maps (right wins) |
| `set` | `(Map(a, b), a, b) -> Map(a, b) where a: Hash` | Insert or update a key |
| `update` | `(Map(a, b), a, b, Fn(b) -> b) -> Map(a, b)` | Update existing or insert default |
| `values` | `(Map(a, b)) -> List(b)` | All values as a list |


## `map.contains`

```
map.contains(m: Map(a, b), key: a) -> Bool where a: Hash
```

Returns `true` if the map has an entry for `key`.

```silt
import map

fn main() {
  let m = #{ "a": 1, "b": 2 }
  println(map.contains(m, "a")) -- true
  println(map.contains(m, "z")) -- false
}
```


## `map.delete`

```
map.delete(m: Map(a, b), key: a) -> Map(a, b) where a: Hash
```

Returns a new map with `key` removed. No-op if key does not exist.

```silt
import map

fn main() {
  let m = #{ "a": 1, "b": 2 }
  let m2 = map.delete(m, "a")
  println(map.length(m2)) -- 1
}
```


## `map.each`

```
map.each(m: Map(a, b), f: Fn(a, b) -> ()) -> ()
```

Calls `f` with each key-value pair. Used for side effects.

```silt
import map

fn main() {
  let m = #{ "x": 10, "y": 20 }
  map.each(m) { k, v -> println("{k} = {v}") }
}
```


## `map.entries`

```
map.entries(m: Map(a, b)) -> List((a, b))
```

Returns all key-value pairs as a list of tuples.

```silt
import map

fn main() {
  let m = #{ "a": 1, "b": 2 }
  let pairs = map.entries(m)
  -- [("a", 1), ("b", 2)]
}
```


## `map.filter`

```
map.filter(m: Map(a, b), f: Fn(a, b) -> Bool) -> Map(a, b)
```

Returns a new map containing only entries where `f` returns `true`.

```silt
import map

fn main() {
  let m = #{ "a": 1, "b": 2, "c": 3 }
  let big = map.filter(m) { k, v -> v > 1 }
  -- #{"b": 2, "c": 3}
}
```


## `map.from_entries`

```
map.from_entries(entries: List((a, b))) -> Map(a, b)
```

Builds a map from a list of `(key, value)` tuples. Later entries overwrite
earlier ones with the same key.

```silt
import map

fn main() {
  let m = map.from_entries([("a", 1), ("b", 2)])
  println(m) -- #{"a": 1, "b": 2}
}
```


## `map.get`

```
map.get(m: Map(a, b), k: a) -> Option(b) where a: Hash
```

Returns `Some(value)` if the key exists, or `None` otherwise.

```silt
import map

fn main() {
  let m = #{ "name": "silt" }
  match map.get(m, "name") {
    Some(v) -> println(v)
    None -> println("not found")
  }
}
```


## `map.keys`

```
map.keys(m: Map(a, b)) -> List(a)
```

Returns all keys as a list, in sorted order.

```silt
import map

fn main() {
  let ks = map.keys(#{ "b": 2, "a": 1 })
  println(ks) -- [a, b]
}
```


## `map.length`

```
map.length(m: Map(a, b)) -> Int
```

Returns the number of entries in the map.

```silt
import map

fn main() {
  println(map.length(#{ "a": 1, "b": 2 })) -- 2
}
```


## `map.map`

```
map.map(m: Map(a, b), f: Fn(a, b) -> (c, d)) -> Map(c, d)
```

Transforms each entry. The callback must return a `(key, value)` tuple.

```silt
import map

fn main() {
  let m = #{ "a": 1, "b": 2 }
  let doubled = map.map(m) { k, v -> (k, v * 2) }
  -- #{"a": 2, "b": 4}
}
```


## `map.merge`

```
map.merge(m1: Map(a, b), m2: Map(a, b)) -> Map(a, b) where a: Hash
```

Merges two maps. When both have the same key, the value from `m2` wins.

```silt
import map

fn main() {
  let a = #{ "x": 1, "y": 2 }
  let b = #{ "y": 99, "z": 3 }
  let merged = map.merge(a, b)
  -- #{"x": 1, "y": 99, "z": 3}
}
```


## `map.set`

```
map.set(m: Map(a, b), k: a, v: b) -> Map(a, b) where a: Hash
```

Returns a new map with the key set to value. Inserts if new, overwrites if
existing.

```silt
import map

fn main() {
  let m = #{ "a": 1 }
  let m2 = map.set(m, "b", 2)
  println(m2) -- #{"a": 1, "b": 2}
}
```


## `map.update`

```
map.update(m: Map(a, b), key: a, default: b, f: Fn(b) -> b) -> Map(a, b)
```

If `key` exists, applies `f` to the current value. If `key` does not exist,
applies `f` to `default`. Inserts the result.

```silt
import map

fn main() {
  let m = #{ "a": 1 }
  let m2 = map.update(m, "a", 0) { v -> v + 10 }
  let m3 = map.update(m2, "b", 0) { v -> v + 10 }
  -- m2 == #{"a": 11}
  -- m3 == #{"a": 11, "b": 10}
}
```


## `map.values`

```
map.values(m: Map(a, b)) -> List(b)
```

Returns all values as a list, in key-sorted order.

```silt
import map

fn main() {
  let vs = map.values(#{ "a": 1, "b": 2 })
  println(vs) -- [1, 2]
}
```
