---
title: "Collections"
section: "Language"
order: 5
---

# Collections

silt has three built-in collection types: **list** (ordered sequence),
**map** (key → value), and **set** (unique elements). All three are
immutable — every "mutating" operation returns a new collection.

## Choosing a collection

| You need …                                                     | Use     |
|----------------------------------------------------------------|---------|
| an ordered sequence, index-addressable, duplicates allowed     | **list** |
| lookup by key, one value per key                               | **map**  |
| membership test, uniqueness, set algebra (union / intersection)| **set**  |
| a fixed-shape heterogeneous record                             | **record** (see [Types](types.md)) |
| a fixed-size heterogeneous sequence                            | **tuple** |

Rule of thumb: if you know the field names at compile time, use a record or
tuple. Reach for a map only when keys are dynamic.

Pattern matching works on lists and maps — see
[Pattern Matching](pattern-matching.md) for `[head, ..tail]` and
`#{ "k": v }`. Sets have no pattern form; test membership with
`set.contains` instead.

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
`list.sort_by`, `list.any`, `list.all`, `list.first`, `list.tail`,
`list.last`, `list.length`, `list.contains`, `list.append`, `list.concat`,
`list.reverse`, `list.get`, `list.take`, `list.drop`, `list.enumerate`,
`list.group_by`, `list.fold_until`, `list.unfold`.

The [list reference](../stdlib/list.md) has every function of `list`, each
with an example; an editor shows the same text when you hover a `list.*`
name.

## Maps

Key-ordered key-value collections with `#{ }`. Iteration visits entries in
sorted key order. Keys can be any comparable type:

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
type Person {
  name: String,
  age: Int,
}
```

**Design rationale.** Heterogeneous maps defeat static typing. If the type
checker cannot know what `map.get(m, key)` returns, it cannot catch errors
at compile time. See [Design Decisions](design-decisions.md#homogeneous-maps)
for the full argument.

Key functions: `map.get`, `map.set`, `map.remove`, `map.contains`,
`map.keys`, `map.values`, `map.entries`, `map.from_entries`, `map.length`,
`map.merge`, `map.filter`, `map.map`, `map.each`, `map.update`.

The [map reference](../stdlib/map.md) has every function of `map`, each
with an example; an editor shows the same text when you hover a `map.*`
name.

## Sets

Sorted unique-value collections with `#[ ]`. Iteration visits elements in
sorted order:

```silt
let tags = #[1, 2, 3]
let words = #["hello", "world", "hello"] -- duplicates removed
```

Set equality with `==`/`!=` works:

```silt
println(#[1, 2, 3] == #[3, 2, 1]) -- true
```

Key functions: `set.from_list`, `set.to_list`, `set.contains`,
`set.insert`, `set.remove`, `set.length`, `set.union`, `set.intersection`,
`set.difference`, `set.is_subset`, `set.map`, `set.filter`, `set.each`,
`set.fold`.

The [set reference](../stdlib/set.md) has every function of `set`, each
with an example; an editor shows the same text when you hover a `set.*`
name.

## How large a collection can be

A list or a set holds at most 10,000,000 elements, and a map at most
10,000,000 entries. What would make a larger one is a runtime error
that names the function and the collection:

```text
list.append: list exceeds maximum size of 10000000 elements
map.set: map exceeds maximum size of 10000000 entries
```

That holds for everything that grows a collection or joins two
(`list.append`, `list.prepend`, `list.concat` and `[..xs, ..ys]`,
`list.flat_map`, `list.flatten`, `list.unfold`, `list.scan`,
`list.zip`, `list.intersperse`, `map.set`, `map.update`, `map.merge`,
`set.insert`, `set.union`, `set.symmetric_difference`,
`stream.collect`, and a `stream.chunks` chunk). A string and a `Bytes`
value hold at most 10,000,000 bytes where they are joined or built
(string interpolation, `string.from`, `string.join`, `string.repeat`,
`string.replace`, `regex.replace`, `regex.replace_first`,
`regex.replace_with`, `encoding.form_encode`, `json.stringify`,
`json.pretty`, `toml.stringify`, `toml.pretty`, `bytes.concat`,
`bytes.concat_all`). One read from a file or stdin (`io.read_file`,
`io.read_bytes`, `io.read_line`, a line of `stream.file_lines`) is as
large as the input, and can be larger. What comes from the network has a limit of its own,
named where the function is (`tcp.read_exact`, a line of
`stream.tcp_lines`, an http body). A channel buffers at most
10,000,000 values (`channel.new`). A program that keeps
adding to a collection in a loop ends with this error instead of
taking the machine's memory. A function that turns text it was given
into a list (`string.split`, `json.parse`) makes as many
elements as the text holds.
