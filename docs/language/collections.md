---
title: "Collections"
---

# Collections

## Lists

Ordered, homogeneous collections:

```silt
let numbers = [1, 2, 3, 4, 5]
```

Spread in list literals with `..`:

```silt
let full = [1, ..tail]
let merged = [..a, 3, ..b]
```

Key functions: `list.map`, `list.filter`, `list.fold`, `list.each`,
`list.find`, `list.zip`, `list.flatten`, `list.flat_map`, `list.filter_map`,
`list.sort_by`, `list.any`, `list.all`, `list.head`, `list.tail`,
`list.last`, `list.length`, `list.contains`, `list.append`, `list.concat`,
`list.reverse`, `list.get`, `list.take`, `list.drop`, `list.enumerate`,
`list.group_by`, `list.fold_until`, `list.unfold`.

## Maps

Unordered key-value collections with `#{ }`. Keys can be any hashable type:

```silt
let config = #{ "host": "localhost", "port": "8080" }
let grid = #{ (0, 0): "start", (1, 2): "end" }
```

Use `map.contains` to check key membership.

**Maps are homogeneous** -- all values must be the same type. This is
enforced by the type system. For heterogeneous data, use records:

```silt
-- ERROR: mixed String and Int values
let m = #{ "name": "Alice", "age": 30 }

-- OK: use a record
type Person { name: String, age: Int }
```

**Design rationale.** Heterogeneous maps defeat static typing. If the type
checker cannot know what `map.get(m, key)` returns, it cannot catch errors
at compile time.

Key functions: `map.get`, `map.set`, `map.delete`, `map.contains`,
`map.keys`, `map.values`, `map.entries`, `map.from_entries`, `map.length`,
`map.merge`, `map.filter`, `map.map`, `map.each`, `map.update`.

## Sets

Unordered unique-value collections with `#[ ]`:

```silt
let tags = #[1, 2, 3]
let words = #["hello", "world", "hello"]   -- duplicates removed
```

Set equality with `==`/`!=` works:

```silt
#[1, 2, 3] == #[3, 2, 1]   -- true
```

Key functions: `set.new`, `set.from_list`, `set.to_list`, `set.contains`,
`set.insert`, `set.remove`, `set.length`, `set.union`, `set.intersection`,
`set.difference`, `set.is_subset`, `set.map`, `set.filter`, `set.each`,
`set.fold`.
