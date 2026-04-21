---
title: "list"
section: "Standard Library"
order: 2
---

# list

Functions for working with ordered, immutable lists (`List(a)`). Lists use
`[...]` literal syntax and support the range operator `1..5`.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `all` | `(List(a), (a) -> Bool) -> Bool` | True if predicate holds for every element |
| `any` | `(List(a), (a) -> Bool) -> Bool` | True if predicate holds for at least one element |
| `append` | `(List(a), a) -> List(a)` | Add an element to the end |
| `concat` | `(List(a), List(a)) -> List(a)` | Concatenate two lists |
| `contains` | `(List(a), a) -> Bool` | Check if element is in list |
| `drop` | `(List(a), Int) -> List(a)` | Remove first n elements |
| `each` | `(List(a), (a) -> ()) -> ()` | Call function for each element (side effects) |
| `enumerate` | `(List(a)) -> List((Int, a))` | Pair each element with its index |
| `filter` | `(List(a), (a) -> Bool) -> List(a)` | Keep elements matching predicate |
| `filter_map` | `(List(a), (a) -> Option(b)) -> List(b)` | Filter and transform in one pass |
| `find` | `(List(a), (a) -> Bool) -> Option(a)` | First element matching predicate |
| `flat_map` | `(List(a), (a) -> List(b)) -> List(b)` | Map then flatten |
| `flatten` | `(List(List(a))) -> List(a)` | Flatten one level of nesting |
| `fold` | `(List(a), b, (b, a) -> b) -> b` | Reduce to a single value |
| `fold_until` | `(List(a), b, (b, a) -> Step(b)) -> b` | Fold with early termination |
| `get` | `(List(a), Int) -> Option(a)` | Element at index, or None |
| `group_by` | `(List(a), (a) -> k) -> Map(k, List(a))` | Group elements by key function |
| `head` | `(List(a)) -> Option(a)` | First element, or None |
| `index_of` | `(List(a), a) -> Option(Int)` | Index of first matching element, or None |
| `intersperse` | `(List(a), a) -> List(a)` | Insert separator between elements |
| `last` | `(List(a)) -> Option(a)` | Last element, or None |
| `length` | `(List(a)) -> Int` | Number of elements |
| `map` | `(List(a), (a) -> b) -> List(b)` | Transform each element |
| `max_by` | `(List(a), (a) -> b) -> Option(a)` | Element with largest key, or None |
| `min_by` | `(List(a), (a) -> b) -> Option(a)` | Element with smallest key, or None |
| `prepend` | `(List(a), a) -> List(a)` | Add an element to the front |
| `product` | `(List(Int)) -> Int` | Product of a list of ints (1 on empty) |
| `product_float` | `(List(Float)) -> Float` | Product of a list of floats (1.0 on empty) |
| `remove_at` | `(List(a), Int) -> List(a)` | Remove element at index (panics if out of range) |
| `reverse` | `(List(a)) -> List(a)` | Reverse element order |
| `scan` | `(List(a), b, (b, a) -> b) -> List(b)` | Prefix fold; returns all intermediate accumulators |
| `set` | `(List(a), Int, a) -> List(a)` | Return new list with element at index replaced |
| `sort` | `(List(a)) -> List(a)` | Sort in natural order |
| `sort_by` | `(List(a), (a) -> b) -> List(a)` | Sort by key function |
| `sum` | `(List(Int)) -> Int` | Sum a list of ints (0 on empty) |
| `sum_float` | `(List(Float)) -> Float` | Sum a list of floats (0.0 on empty) |
| `tail` | `(List(a)) -> List(a)` | All elements except the first |
| `take` | `(List(a), Int) -> List(a)` | Keep first n elements |
| `unfold` | `(a, (a) -> Option((b, a))) -> List(b)` | Build a list from a seed |
| `unique` | `(List(a)) -> List(a)` | Remove duplicates, preserving first occurrence |
| `zip` | `(List(a), List(b)) -> List((a, b))` | Pair elements from two lists |


## `list.all`

```
list.all(xs: List(a), f: (a) -> Bool) -> Bool
```

Returns `true` if `f` returns `true` for every element. Short-circuits on the
first `false`.

```silt
import list
fn main() {
    let all_even = list.all([2, 4, 6]) { x -> x % 2 == 0 }
    println(all_even)  -- true
}
```


## `list.any`

```
list.any(xs: List(a), f: (a) -> Bool) -> Bool
```

Returns `true` if `f` returns `true` for at least one element. Short-circuits on
the first `true`.

```silt
import list
fn main() {
    let has_even = list.any([1, 3, 4]) { x -> x % 2 == 0 }
    println(has_even)  -- true
}
```


## `list.append`

```
list.append(xs: List(a), elem: a) -> List(a)
```

Returns a new list with `elem` added at the end.

```silt
import list
fn main() {
    let xs = [1, 2, 3] |> list.append(4)
    println(xs)  -- [1, 2, 3, 4]
}
```


## `list.concat`

```
list.concat(xs: List(a), ys: List(a)) -> List(a)
```

Concatenates two lists into a single list.

```silt
import list
fn main() {
    let joined = list.concat([1, 2], [3, 4])
    println(joined)  -- [1, 2, 3, 4]
}
```


## `list.contains`

```
list.contains(xs: List(a), elem: a) -> Bool
```

Returns `true` if `elem` is in the list (by value equality).

```silt
import list
fn main() {
    println(list.contains([1, 2, 3], 2))  -- true
    println(list.contains([1, 2, 3], 5))  -- false
}
```


## `list.drop`

```
list.drop(xs: List(a), n: Int) -> List(a)
```

Returns the list without its first `n` elements. If `n >= length`, returns an
empty list. Negative `n` is a runtime error.

```silt
import list
fn main() {
    let tail = list.drop([1, 2, 3, 4, 5], 2)
    println(tail)  -- [3, 4, 5]
}
```


## `list.each`

```
list.each(xs: List(a), f: (a) -> ()) -> ()
```

Calls `f` for every element in the list. Used for side effects. Returns unit.

```silt
import list
fn main() {
    [1, 2, 3] |> list.each { x -> println(x) }
}
```


## `list.enumerate`

```
list.enumerate(xs: List(a)) -> List((Int, a))
```

Returns a list of `(index, element)` tuples, with indices starting at 0.

```silt
import list
fn main() {
    let pairs = list.enumerate(["a", "b", "c"])
    -- [(0, "a"), (1, "b"), (2, "c")]
    list.each(pairs) { (i, v) -> println("{i}: {v}") }
}
```


## `list.filter`

```
list.filter(xs: List(a), f: (a) -> Bool) -> List(a)
```

Returns a list containing only the elements for which `f` returns `true`.

```silt
import list
fn main() {
    let evens = [1, 2, 3, 4, 5] |> list.filter { x -> x % 2 == 0 }
    println(evens)  -- [2, 4]
}
```


## `list.filter_map`

```
list.filter_map(xs: List(a), f: (a) -> Option(b)) -> List(b)
```

Applies `f` to each element. Keeps the inner values from `Some` results and
discards `None` results. Combines filtering and mapping in one pass.

```silt
import int

import list
fn main() {
    let results = ["1", "abc", "3"] |> list.filter_map { s ->
        match int.parse(s) {
            Ok(n) -> Some(n * 10)
            Err(_) -> None
        }
    }
    println(results)  -- [10, 30]
}
```


## `list.find`

```
list.find(xs: List(a), f: (a) -> Bool) -> Option(a)
```

Returns `Some(element)` for the first element where `f` returns `true`, or
`None` if no match is found.

```silt
import list
fn main() {
    let first_gt_2 = list.find([1, 2, 3, 4]) { x -> x > 2 }
    println(first_gt_2)  -- Some(3)
}
```


## `list.flat_map`

```
list.flat_map(xs: List(a), f: (a) -> List(b)) -> List(b)
```

Maps each element to a list, then flattens the results into a single list.

```silt
import list
fn main() {
    let expanded = [1, 2, 3] |> list.flat_map { x -> [x, x * 10] }
    println(expanded)  -- [1, 10, 2, 20, 3, 30]
}
```


## `list.flatten`

```
list.flatten(xs: List(List(a))) -> List(a)
```

Flattens one level of nesting. Non-list elements are kept as-is.

```silt
import list
fn main() {
    let flat = list.flatten([[1, 2], [3], [4, 5]])
    println(flat)  -- [1, 2, 3, 4, 5]
}
```


## `list.fold`

```
list.fold(xs: List(a), init: b, f: (b, a) -> b) -> b
```

Reduces a list to a single value. Starts with `init`, then calls `f(acc, elem)`
for each element.

```silt
import list
fn main() {
    let sum = [1, 2, 3] |> list.fold(0) { acc, x -> acc + x }
    println(sum)  -- 6
}
```


## `list.fold_until`

```
list.fold_until(xs: List(a), init: b, f: (b, a) -> Step(b)) -> b
```

Like `fold`, but the callback returns `Continue(acc)` to keep going or
`Stop(value)` to terminate early.

```silt
import list
fn main() {
    -- Sum until we exceed 5
    let partial_sum = list.fold_until([1, 2, 3, 4, 5], 0) { acc, x ->
        let next = acc + x
        match {
            next > 5 -> Stop(acc)
            _ -> Continue(next)
        }
    }
    println(partial_sum)  -- 3
}
```


## `list.get`

```
list.get(xs: List(a), index: Int) -> Option(a)
```

Returns `Some(element)` at the given index, or `None` if out of bounds.
Negative indices are a runtime error -- use `list.last` for end access.

```silt
import list
fn main() {
    let xs = [10, 20, 30]
    println(list.get(xs, 1))   -- Some(20)
    println(list.get(xs, 10))  -- None
    -- list.get(xs, -1)        -- runtime error: negative index
}
```


## `list.group_by`

```
list.group_by(xs: List(a), f: (a) -> k) -> Map(k, List(a))
```

Groups elements by the result of applying `f`. Returns a map from keys to lists
of elements that produced that key.

```silt
import list
fn main() {
    let groups = [1, 2, 3, 4, 5, 6] |> list.group_by { x -> x % 2 }
    -- #{0: [2, 4, 6], 1: [1, 3, 5]}
}
```


## `list.head`

```
list.head(xs: List(a)) -> Option(a)
```

Returns `Some(first_element)` or `None` if the list is empty.

```silt
import list
fn main() {
    println(list.head([1, 2, 3]))  -- Some(1)
    println(list.head([]))         -- None
}
```


## `list.index_of`

```
list.index_of(xs: List(a), target: a) -> Option(Int)
```

Returns `Some(index)` of the first element equal to `target` (by value
equality), or `None` if no element matches.

```silt
import list
fn main() {
    println(list.index_of([10, 20, 30, 20], 20))  -- Some(1)
    println(list.index_of([10, 20, 30], 99))      -- None
}
```


## `list.intersperse`

```
list.intersperse(xs: List(a), sep: a) -> List(a)
```

Returns a new list with `sep` inserted between consecutive elements. Empty and
single-element inputs are returned unchanged.

```silt
import list
fn main() {
    println(list.intersperse([1, 2, 3], 0))  -- [1, 0, 2, 0, 3]
    println(list.intersperse([42], 0))       -- [42]
    println(list.intersperse([], 0))         -- []
}
```


## `list.last`

```
list.last(xs: List(a)) -> Option(a)
```

Returns `Some(last_element)` or `None` if the list is empty.

```silt
import list
fn main() {
    println(list.last([1, 2, 3]))  -- Some(3)
    println(list.last([]))         -- None
}
```


## `list.length`

```
list.length(xs: List(a)) -> Int
```

Returns the number of elements in the list.

```silt
import list
fn main() {
    println(list.length([1, 2, 3]))  -- 3
    println(list.length([]))         -- 0
}
```


## `list.map`

```
list.map(xs: List(a), f: (a) -> b) -> List(b)
```

Returns a new list with `f` applied to each element.

```silt
import list
fn main() {
    let doubled = [1, 2, 3] |> list.map { x -> x * 2 }
    println(doubled)  -- [2, 4, 6]
}
```


## `list.max_by`

```
list.max_by(xs: List(a), key: (a) -> b) -> Option(a)
```

Returns `Some(element)` whose `key` result is largest, or `None` if the list
is empty. On ties, returns the first element with the maximum key. Requires
`b` to support comparison (numbers, strings, etc.).

```silt
import list
import string
fn main() {
    let words = ["fig", "banana", "apple"]
    let longest = list.max_by(words) { w -> string.length(w) }
    println(longest)  -- Some("banana")
    println(list.max_by([], { x -> x }))  -- None
}
```


## `list.min_by`

```
list.min_by(xs: List(a), key: (a) -> b) -> Option(a)
```

Returns `Some(element)` whose `key` result is smallest, or `None` if the list
is empty. On ties, returns the first element with the minimum key. Requires
`b` to support comparison (numbers, strings, etc.).

```silt
import list
import string
fn main() {
    let words = ["banana", "fig", "apple"]
    let shortest = list.min_by(words) { w -> string.length(w) }
    println(shortest)  -- Some("fig")
    println(list.min_by([], { x -> x }))  -- None
}
```


## `list.prepend`

```
list.prepend(xs: List(a), elem: a) -> List(a)
```

Returns a new list with `elem` added at the front.

```silt
import list
fn main() {
    let xs = [2, 3] |> list.prepend(1)
    println(xs)  -- [1, 2, 3]
}
```


## `list.product`

```
list.product(xs: List(Int)) -> Int
```

Returns the product of all elements. Returns `1` on an empty list. Overflow
is a runtime error.

```silt
import list
fn main() {
    println(list.product([1, 2, 3, 4]))  -- 24
    println(list.product([]))            -- 1
}
```


## `list.product_float`

```
list.product_float(xs: List(Float)) -> Float
```

Like `product`, but for lists of floats. Returns `1.0` on an empty list.

```silt
import list
fn main() {
    println(list.product_float([1.5, 2.0, 4.0]))  -- 12.0
    println(list.product_float([]))               -- 1.0
}
```


## `list.remove_at`

```
list.remove_at(xs: List(a), index: Int) -> List(a)
```

Returns a new list with the element at `index` removed. Panics if the index is
out of bounds (matching `list.set`). Negative indices are a runtime error.

```silt
import list
fn main() {
    println(list.remove_at([10, 20, 30, 40], 1))  -- [10, 30, 40]
    -- list.remove_at([1, 2], 5)                   -- runtime error: out of bounds
}
```


## `list.reverse`

```
list.reverse(xs: List(a)) -> List(a)
```

Returns a new list with elements in reverse order.

```silt
import list
fn main() {
    println(list.reverse([1, 2, 3]))  -- [3, 2, 1]
}
```


## `list.set`

```
list.set(xs: List(a), index: Int, value: a) -> List(a)
```

Returns a new list with the element at `index` replaced by `value`. Panics if
the index is out of bounds. Negative indices are a runtime error.

```silt
import list
fn main() {
    let xs = list.set([10, 20, 30], 1, 99)
    println(xs)  -- [10, 99, 30]
}
```


## `list.sort`

```
list.sort(xs: List(a)) -> List(a)
```

Returns a new list sorted in natural (ascending) order.

```silt
import list
fn main() {
    println(list.sort([3, 1, 2]))  -- [1, 2, 3]
}
```


## `list.scan`

```
list.scan(xs: List(a), init: b, f: (b, a) -> b) -> List(b)
```

Like `fold`, but returns every intermediate accumulator rather than just the
final one. The result length is `length(xs) + 1`: the first element is `init`,
and each subsequent element is the accumulator after applying `f` to the next
input. This matches Haskell's `scanl` and Rust's `std::iter::successors`
(in spirit) — the convention is **inclusive of the initial value**.

```silt
import list
fn main() {
    -- Prefix sums: [0, 1, 3, 6, 10]
    let sums = list.scan([1, 2, 3, 4], 0) { acc, x -> acc + x }
    println(sums)
    println(list.scan([], 0) { acc, x -> acc + x })  -- [0]
}
```


## `list.sort_by`

```
list.sort_by(xs: List(a), key: (a) -> b) -> List(a)
```

Returns a new list sorted by the result of applying the key function to each
element.

```silt
import list
import string
fn main() {
    let words = ["banana", "fig", "apple"]
    let sorted = words |> list.sort_by { w -> string.length(w) }
    println(sorted)  -- ["fig", "apple", "banana"]
}
```


## `list.sum`

```
list.sum(xs: List(Int)) -> Int
```

Returns the sum of all elements. Returns `0` on an empty list. Overflow
is a runtime error.

```silt
import list
fn main() {
    println(list.sum([1, 2, 3, 4]))  -- 10
    println(list.sum([]))            -- 0
}
```


## `list.sum_float`

```
list.sum_float(xs: List(Float)) -> Float
```

Like `sum`, but for lists of floats. Returns `0.0` on an empty list.

```silt
import list
fn main() {
    println(list.sum_float([0.5, 1.5, 2.0]))  -- 4.0
    println(list.sum_float([]))               -- 0.0
}
```


## `list.tail`

```
list.tail(xs: List(a)) -> List(a)
```

Returns all elements except the first. Returns an empty list if the input is
empty.

```silt
import list
fn main() {
    println(list.tail([1, 2, 3]))  -- [2, 3]
    println(list.tail([]))         -- []
}
```


## `list.take`

```
list.take(xs: List(a), n: Int) -> List(a)
```

Returns the first `n` elements. If `n >= length`, returns the whole list.
Negative `n` is a runtime error.

```silt
import list
fn main() {
    println(list.take([1, 2, 3, 4, 5], 3))  -- [1, 2, 3]
}
```


## `list.unfold`

```
list.unfold(seed: a, f: (a) -> Option((b, a))) -> List(b)
```

Builds a list from a seed value. The function returns `Some((element, next_seed))`
to emit an element and continue, or `None` to stop.

```silt
import list
fn main() {
    let countdown = list.unfold(5) { n ->
        match {
            n <= 0 -> None
            _ -> Some((n, n - 1))
        }
    }
    println(countdown)  -- [5, 4, 3, 2, 1]
}
```


## `list.unique`

```
list.unique(xs: List(a)) -> List(a)
```

Removes duplicate elements, preserving the order of first occurrences.

```silt
import list
fn main() {
    println(list.unique([1, 2, 1, 3, 2]))  -- [1, 2, 3]
}
```


## `list.zip`

```
list.zip(xs: List(a), ys: List(b)) -> List((a, b))
```

Pairs up elements from two lists. Stops at the shorter list.

```silt
import list
fn main() {
    let pairs = list.zip([1, 2, 3], ["a", "b", "c"])
    println(pairs)  -- [(1, "a"), (2, "b"), (3, "c")]
}
```
