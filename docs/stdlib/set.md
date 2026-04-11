---
title: "set"
section: "Standard Library"
order: 5
---

# set

Functions for working with immutable, ordered sets (`Set(a)`). Sets use `#[...]`
literal syntax and contain unique values.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `contains` | `(Set(a), a) -> Bool` | Check membership |
| `difference` | `(Set(a), Set(a)) -> Set(a)` | Elements in first but not second |
| `each` | `(Set(a), (a) -> ()) -> ()` | Iterate over all elements |
| `filter` | `(Set(a), (a) -> Bool) -> Set(a)` | Keep elements matching predicate |
| `fold` | `(Set(a), b, (b, a) -> b) -> b` | Reduce to a single value |
| `from_list` | `(List(a)) -> Set(a)` | Create set from list |
| `insert` | `(Set(a), a) -> Set(a)` | Add an element |
| `intersection` | `(Set(a), Set(a)) -> Set(a)` | Elements in both sets |
| `is_subset` | `(Set(a), Set(a)) -> Bool` | True if first is subset of second |
| `length` | `(Set(a)) -> Int` | Number of elements |
| `map` | `(Set(a), (a) -> b) -> Set(b)` | Transform each element |
| `new` | `() -> Set(a)` | Create an empty set |
| `remove` | `(Set(a), a) -> Set(a)` | Remove an element |
| `to_list` | `(Set(a)) -> List(a)` | Convert set to sorted list |
| `union` | `(Set(a), Set(a)) -> Set(a)` | Combine all elements |


## `set.contains`

```
set.contains(s: Set(a), elem: a) -> Bool
```

Returns `true` if `elem` is in the set.

```silt
import set
fn main() {
    let s = #[1, 2, 3]
    println(set.contains(s, 2))  -- true
    println(set.contains(s, 5))  -- false
}
```


## `set.difference`

```
set.difference(a: Set(a), b: Set(a)) -> Set(a)
```

Returns elements that are in `a` but not in `b`.

```silt
import set
fn main() {
    let result = set.difference(#[1, 2, 3], #[2, 3, 4])
    println(set.to_list(result))  -- [1]
}
```


## `set.each`

```
set.each(s: Set(a), f: (a) -> ()) -> ()
```

Calls `f` for every element. Used for side effects.

```silt
import set
fn main() {
    set.each(#[1, 2, 3]) { x -> println(x) }
}
```


## `set.filter`

```
set.filter(s: Set(a), f: (a) -> Bool) -> Set(a)
```

Returns a new set containing only elements for which `f` returns `true`.

```silt
import set
fn main() {
    let evens = set.filter(#[1, 2, 3, 4]) { x -> x % 2 == 0 }
    println(set.to_list(evens))  -- [2, 4]
}
```


## `set.fold`

```
set.fold(s: Set(a), init: b, f: (b, a) -> b) -> b
```

Reduces the set to a single value. Iteration order is sorted.

```silt
import set
fn main() {
    let sum = set.fold(#[1, 2, 3], 0) { acc, x -> acc + x }
    println(sum)  -- 6
}
```


## `set.from_list`

```
set.from_list(xs: List(a)) -> Set(a)
```

Creates a set from a list, removing duplicates.

```silt
import set
fn main() {
    let s = set.from_list([1, 2, 2, 3])
    println(set.length(s))  -- 3
}
```


## `set.insert`

```
set.insert(s: Set(a), elem: a) -> Set(a)
```

Returns a new set with `elem` added. No-op if already present.

```silt
import set
fn main() {
    let s = set.insert(#[1, 2], 3)
    println(set.to_list(s))  -- [1, 2, 3]
}
```


## `set.intersection`

```
set.intersection(a: Set(a), b: Set(a)) -> Set(a)
```

Returns elements that are in both `a` and `b`.

```silt
import set
fn main() {
    let result = set.intersection(#[1, 2, 3], #[2, 3, 4])
    println(set.to_list(result))  -- [2, 3]
}
```


## `set.is_subset`

```
set.is_subset(a: Set(a), b: Set(a)) -> Bool
```

Returns `true` if every element of `a` is also in `b`.

```silt
import set
fn main() {
    println(set.is_subset(#[1, 2], #[1, 2, 3]))  -- true
    println(set.is_subset(#[1, 4], #[1, 2, 3]))  -- false
}
```


## `set.length`

```
set.length(s: Set(a)) -> Int
```

Returns the number of elements in the set.

```silt
import set
fn main() {
    println(set.length(#[1, 2, 3]))  -- 3
}
```


## `set.map`

```
set.map(s: Set(a), f: (a) -> b) -> Set(b)
```

Returns a new set with `f` applied to each element. The result set may be
smaller if `f` maps distinct elements to the same value.

```silt
import set
fn main() {
    let result = set.map(#[1, 2, 3]) { x -> x * 10 }
    println(set.to_list(result))  -- [10, 20, 30]
}
```


## `set.new`

```
set.new() -> Set(a)
```

Creates a new empty set.

```silt
import set
fn main() {
    let s = set.new()
    let s = set.insert(s, 42)
    println(set.length(s))  -- 1
}
```


## `set.remove`

```
set.remove(s: Set(a), elem: a) -> Set(a)
```

Returns a new set with `elem` removed. No-op if not present.

```silt
import set
fn main() {
    let s = set.remove(#[1, 2, 3], 2)
    println(set.to_list(s))  -- [1, 3]
}
```


## `set.to_list`

```
set.to_list(s: Set(a)) -> List(a)
```

Converts the set to a sorted list.

```silt
import set
fn main() {
    let xs = set.to_list(#[3, 1, 2])
    println(xs)  -- [1, 2, 3]
}
```


## `set.union`

```
set.union(a: Set(a), b: Set(a)) -> Set(a)
```

Returns a set containing all elements from both `a` and `b`.

```silt
import set
fn main() {
    let result = set.union(#[1, 2], #[2, 3])
    println(set.to_list(result))  -- [1, 2, 3]
}
```
