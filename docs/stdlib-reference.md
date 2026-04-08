---
title: "Standard Library"
order: 3
description: "Every builtin function in silt's stdlib with signatures and examples — list, string, map, set, io, regex, json, and more."
---

# Silt Standard Library Reference

Complete API reference for every built-in function in silt.

## Module Index

| Module | Functions | Description |
|--------|:---------:|-------------|
| [Globals](#globals) | 12 | `print`, `println`, `panic`, variant constructors, type descriptors |
| [list](#list) | 31 | Create, transform, query, and iterate over ordered collections |
| [string](#string) | 27 | Split, join, search, transform, and classify strings |
| [map](#map) | 14 | Lookup, insert, merge, and iterate over key-value maps |
| [set](#set) | 15 | Create, combine, query, and iterate over unordered unique collections |
| [int](#int) | 6 | Parse, convert, and compare integers |
| [float](#float) | 9 | Parse, round, convert, and compare floats |
| [result](#result) | 7 | Transform and query `Result(a, e)` values |
| [option](#option) | 6 | Transform and query `Option(a)` values |
| [io](#io) | 5 | File I/O, stdin, command-line args, debug inspection |
| [fs](#fs) | 1 | Filesystem path queries |
| [test](#test) | 3 | Assertions for test scripts |
| [regex](#regex) | 9 | Match, find, split, replace, and capture with regular expressions |
| [json](#json) | 5 | Parse JSON into typed records/maps, serialize values to JSON |
| [math](#math) | 11 + 2 | Trigonometry, logarithms, exponentiation, and constants |
| [channel](#channel) | 8 | Bounded channels for concurrent task communication |
| [task](#task) | 3 | Spawn, join, and cancel lightweight tasks |
| [time](#time) | 26 | Dates, times, instants, durations, formatting, parsing, and arithmetic |
| [http](#http) | 4 | HTTP client and server |

**Total: 199 names** (19 globals + 4 type descriptors + 176 module functions/constants)


## Globals

Always available. No import or qualification needed.

### Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `print` | `(a) -> ()` | Print a value without trailing newline |
| `println` | `(a) -> ()` | Print a value with trailing newline |
| `panic` | `(String) -> a` | Crash with an error message |
| `Ok` | `(a) -> Result(a, e)` | Construct a success Result |
| `Err` | `(e) -> Result(a, e)` | Construct an error Result |
| `Some` | `(a) -> Option(a)` | Construct a present Option |
| `None` | `Option(a)` | The absent Option value (not a function) |
| `Stop` | `(a) -> Step(a)` | Signal early termination in `list.fold_until` |
| `Continue` | `(a) -> Step(a)` | Signal continuation in `list.fold_until` |
| `Message` | `(a) -> ChannelResult(a)` | Wraps a received channel value |
| `Closed` | `ChannelResult(a)` | Channel is closed |
| `Empty` | `ChannelResult(a)` | Channel buffer empty (non-blocking receive) |
| `Sent` | `ChannelResult(a)` | Value was sent successfully (select send) |
| `Monday`..`Sunday` | `Weekday` | Day-of-week constructors (require `import time`) |

Additionally, four **type descriptors** are in the global namespace for use with
`json.parse_map` and similar type-directed APIs:

| Name | Description |
|------|-------------|
| `Int` | Integer type descriptor |
| `Float` | Float type descriptor |
| `String` | String type descriptor |
| `Bool` | Boolean type descriptor |


### `print`

```
print(value: a) -> ()
```

Prints a value to stdout. Does not append a newline. Multiple values in a single
call are separated by spaces.

```silt
fn main() {
    print("hello ")
    print("world")
    // output: hello world
}
```


### `println`

```
println(value: a) -> ()
```

Prints a value to stdout followed by a newline.

```silt
fn main() {
    println("hello, world")
    // output: hello, world\n
}
```


### `panic`

```
panic(message: String) -> a
```

Terminates execution with an error message. The return type is polymorphic
because `panic` never returns -- it can appear anywhere a value is expected.

```silt
fn main() {
    panic("something went wrong")
}
```


### `Ok`

```
Ok(value: a) -> Result(a, e)
```

Constructs a success variant of `Result`.

```silt
fn main() {
    let r = Ok(42)
    // r is Result(Int, e)
}
```


### `Err`

```
Err(error: e) -> Result(a, e)
```

Constructs an error variant of `Result`.

```silt
fn main() {
    let r = Err("not found")
    // r is Result(a, String)
}
```


### `Some`

```
Some(value: a) -> Option(a)
```

Constructs a present variant of `Option`.

```silt
fn main() {
    let x = Some(42)
    match x {
        Some(n) -> println(n)
        None -> println("nothing")
    }
}
```


### `None`

```
None : Option(a)
```

The absent variant of `Option`. This is a value, not a function.

```silt
fn main() {
    let x = None
    println(option.is_none(x))  // true
}
```


### `Stop`

```
Stop(value: a) -> Step(a)
```

Signals early termination from `list.fold_until`. The value becomes the final
accumulator result.

```silt
fn main() {
    let result = list.fold_until([1, 2, 3, 4, 5], 0) { acc, x ->
        when acc + x > 6 -> Stop(acc)
        else -> Continue(acc + x)
    }
    println(result)  // 6
}
```


### `Continue`

```
Continue(value: a) -> Step(a)
```

Signals continuation in `list.fold_until`. The value becomes the next
accumulator.


### `Message`

```
Message(value: a) -> ChannelResult(a)
```

Wraps a value received from a channel. Returned by `channel.receive` and
`channel.try_receive` when a value is available.

```silt
fn main() {
    let ch = channel.new(1)
    channel.send(ch, 42)
    let Message(v) = channel.receive(ch)
    println(v)  // 42
}
```


### `Closed`

```
Closed : ChannelResult(a)
```

Indicates the channel has been closed. Returned by `channel.receive` and
`channel.try_receive` when no more messages will arrive.


### `Empty`

```
Empty : ChannelResult(a)
```

Indicates the channel buffer is currently empty but not closed. Only returned by
`channel.try_receive` (the non-blocking variant).


### `Sent`

```
Sent : ChannelResult(a)
```

Indicates a value was successfully sent to a channel. Returned by
`channel.select` when a send operation completes.

```silt
fn main() {
    let ch = channel.new(1)
    match channel.select([(ch, 42)]) {
        (_, Sent) -> println("sent!")
        (_, Closed) -> println("closed")
    }
}
```


## list

Functions for working with ordered, immutable lists (`List(a)`). Lists use
`[...]` literal syntax and support the range operator `1..5`.

### Summary

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
| `last` | `(List(a)) -> Option(a)` | Last element, or None |
| `length` | `(List(a)) -> Int` | Number of elements |
| `map` | `(List(a), (a) -> b) -> List(b)` | Transform each element |
| `prepend` | `(List(a), a) -> List(a)` | Add an element to the front |
| `reverse` | `(List(a)) -> List(a)` | Reverse element order |
| `set` | `(List(a), Int, a) -> List(a)` | Return new list with element at index replaced |
| `sort` | `(List(a)) -> List(a)` | Sort in natural order |
| `sort_by` | `(List(a), (a) -> b) -> List(a)` | Sort by key function |
| `tail` | `(List(a)) -> List(a)` | All elements except the first |
| `take` | `(List(a), Int) -> List(a)` | Keep first n elements |
| `unfold` | `(a, (a) -> Option((b, a))) -> List(b)` | Build a list from a seed |
| `unique` | `(List(a)) -> List(a)` | Remove duplicates, preserving first occurrence |
| `zip` | `(List(a), List(b)) -> List((a, b))` | Pair elements from two lists |


### `list.all`

```
list.all(xs: List(a), f: (a) -> Bool) -> Bool
```

Returns `true` if `f` returns `true` for every element. Short-circuits on the
first `false`.

```silt
fn main() {
    let result = list.all([2, 4, 6]) { x -> x % 2 == 0 }
    println(result)  // true
}
```


### `list.any`

```
list.any(xs: List(a), f: (a) -> Bool) -> Bool
```

Returns `true` if `f` returns `true` for at least one element. Short-circuits on
the first `true`.

```silt
fn main() {
    let result = list.any([1, 3, 4]) { x -> x % 2 == 0 }
    println(result)  // true
}
```


### `list.append`

```
list.append(xs: List(a), elem: a) -> List(a)
```

Returns a new list with `elem` added at the end.

```silt
fn main() {
    let xs = [1, 2, 3] |> list.append(4)
    println(xs)  // [1, 2, 3, 4]
}
```


### `list.concat`

```
list.concat(xs: List(a), ys: List(a)) -> List(a)
```

Concatenates two lists into a single list.

```silt
fn main() {
    let result = list.concat([1, 2], [3, 4])
    println(result)  // [1, 2, 3, 4]
}
```


### `list.contains`

```
list.contains(xs: List(a), elem: a) -> Bool
```

Returns `true` if `elem` is in the list (by value equality).

```silt
fn main() {
    println(list.contains([1, 2, 3], 2))  // true
    println(list.contains([1, 2, 3], 5))  // false
}
```


### `list.drop`

```
list.drop(xs: List(a), n: Int) -> List(a)
```

Returns the list without its first `n` elements. If `n >= length`, returns an
empty list. Negative `n` is a runtime error.

```silt
fn main() {
    let result = list.drop([1, 2, 3, 4, 5], 2)
    println(result)  // [3, 4, 5]
}
```


### `list.each`

```
list.each(xs: List(a), f: (a) -> ()) -> ()
```

Calls `f` for every element in the list. Used for side effects. Returns unit.

```silt
fn main() {
    [1, 2, 3] |> list.each { x -> println(x) }
}
```


### `list.enumerate`

```
list.enumerate(xs: List(a)) -> List((Int, a))
```

Returns a list of `(index, element)` tuples, with indices starting at 0.

```silt
fn main() {
    let pairs = list.enumerate(["a", "b", "c"])
    // [(0, "a"), (1, "b"), (2, "c")]
    list.each(pairs) { (i, v) -> println("{i}: {v}") }
}
```


### `list.filter`

```
list.filter(xs: List(a), f: (a) -> Bool) -> List(a)
```

Returns a list containing only the elements for which `f` returns `true`.

```silt
fn main() {
    let evens = [1, 2, 3, 4, 5] |> list.filter { x -> x % 2 == 0 }
    println(evens)  // [2, 4]
}
```


### `list.filter_map`

```
list.filter_map(xs: List(a), f: (a) -> Option(b)) -> List(b)
```

Applies `f` to each element. Keeps the inner values from `Some` results and
discards `None` results. Combines filtering and mapping in one pass.

```silt
fn main() {
    let results = ["1", "abc", "3"] |> list.filter_map { s ->
        match int.parse(s) {
            Ok(n) -> Some(n * 10)
            Err(_) -> None
        }
    }
    println(results)  // [10, 30]
}
```


### `list.find`

```
list.find(xs: List(a), f: (a) -> Bool) -> Option(a)
```

Returns `Some(element)` for the first element where `f` returns `true`, or
`None` if no match is found.

```silt
fn main() {
    let result = list.find([1, 2, 3, 4]) { x -> x > 2 }
    println(result)  // Some(3)
}
```


### `list.flat_map`

```
list.flat_map(xs: List(a), f: (a) -> List(b)) -> List(b)
```

Maps each element to a list, then flattens the results into a single list.

```silt
fn main() {
    let result = [1, 2, 3] |> list.flat_map { x -> [x, x * 10] }
    println(result)  // [1, 10, 2, 20, 3, 30]
}
```


### `list.flatten`

```
list.flatten(xs: List(List(a))) -> List(a)
```

Flattens one level of nesting. Non-list elements are kept as-is.

```silt
fn main() {
    let result = list.flatten([[1, 2], [3], [4, 5]])
    println(result)  // [1, 2, 3, 4, 5]
}
```


### `list.fold`

```
list.fold(xs: List(a), init: b, f: (b, a) -> b) -> b
```

Reduces a list to a single value. Starts with `init`, then calls `f(acc, elem)`
for each element.

```silt
fn main() {
    let sum = [1, 2, 3] |> list.fold(0) { acc, x -> acc + x }
    println(sum)  // 6
}
```


### `list.fold_until`

```
list.fold_until(xs: List(a), init: b, f: (b, a) -> Step(b)) -> b
```

Like `fold`, but the callback returns `Continue(acc)` to keep going or
`Stop(value)` to terminate early.

```silt
fn main() {
    // Sum until we exceed 5
    let result = list.fold_until([1, 2, 3, 4, 5], 0) { acc, x ->
        let next = acc + x
        when next > 5 -> Stop(acc)
        else -> Continue(next)
    }
    println(result)  // 3
}
```


### `list.get`

```
list.get(xs: List(a), index: Int) -> Option(a)
```

Returns `Some(element)` at the given index, or `None` if out of bounds.
Negative indices are a runtime error -- use `list.last` for end access.

```silt
fn main() {
    let xs = [10, 20, 30]
    println(list.get(xs, 1))   // Some(20)
    println(list.get(xs, 10))  // None
    -- list.get(xs, -1)        -- runtime error: negative index
}
```


### `list.group_by`

```
list.group_by(xs: List(a), f: (a) -> k) -> Map(k, List(a))
```

Groups elements by the result of applying `f`. Returns a map from keys to lists
of elements that produced that key.

```silt
fn main() {
    let groups = [1, 2, 3, 4, 5, 6] |> list.group_by { x -> x % 2 }
    // #{0: [2, 4, 6], 1: [1, 3, 5]}
}
```


### `list.head`

```
list.head(xs: List(a)) -> Option(a)
```

Returns `Some(first_element)` or `None` if the list is empty.

```silt
fn main() {
    println(list.head([1, 2, 3]))  // Some(1)
    println(list.head([]))         // None
}
```


### `list.last`

```
list.last(xs: List(a)) -> Option(a)
```

Returns `Some(last_element)` or `None` if the list is empty.

```silt
fn main() {
    println(list.last([1, 2, 3]))  // Some(3)
    println(list.last([]))         // None
}
```


### `list.length`

```
list.length(xs: List(a)) -> Int
```

Returns the number of elements in the list.

```silt
fn main() {
    println(list.length([1, 2, 3]))  // 3
    println(list.length([]))         // 0
}
```


### `list.map`

```
list.map(xs: List(a), f: (a) -> b) -> List(b)
```

Returns a new list with `f` applied to each element.

```silt
fn main() {
    let doubled = [1, 2, 3] |> list.map { x -> x * 2 }
    println(doubled)  // [2, 4, 6]
}
```


### `list.prepend`

```
list.prepend(xs: List(a), elem: a) -> List(a)
```

Returns a new list with `elem` added at the front.

```silt
fn main() {
    let xs = [2, 3] |> list.prepend(1)
    println(xs)  // [1, 2, 3]
}
```


### `list.reverse`

```
list.reverse(xs: List(a)) -> List(a)
```

Returns a new list with elements in reverse order.

```silt
fn main() {
    println(list.reverse([1, 2, 3]))  // [3, 2, 1]
}
```


### `list.set`

```
list.set(xs: List(a), index: Int, value: a) -> List(a)
```

Returns a new list with the element at `index` replaced by `value`. Panics if
the index is out of bounds. Negative indices are a runtime error.

```silt
fn main() {
    let xs = list.set([10, 20, 30], 1, 99)
    println(xs)  // [10, 99, 30]
}
```


### `list.sort`

```
list.sort(xs: List(a)) -> List(a)
```

Returns a new list sorted in natural (ascending) order.

```silt
fn main() {
    println(list.sort([3, 1, 2]))  // [1, 2, 3]
}
```


### `list.sort_by`

```
list.sort_by(xs: List(a), key: (a) -> b) -> List(a)
```

Returns a new list sorted by the result of applying the key function to each
element.

```silt
fn main() {
    let words = ["banana", "fig", "apple"]
    let sorted = words |> list.sort_by { w -> string.length(w) }
    println(sorted)  // ["fig", "apple", "banana"]
}
```


### `list.tail`

```
list.tail(xs: List(a)) -> List(a)
```

Returns all elements except the first. Returns an empty list if the input is
empty.

```silt
fn main() {
    println(list.tail([1, 2, 3]))  // [2, 3]
    println(list.tail([]))         // []
}
```


### `list.take`

```
list.take(xs: List(a), n: Int) -> List(a)
```

Returns the first `n` elements. If `n >= length`, returns the whole list.
Negative `n` is a runtime error.

```silt
fn main() {
    println(list.take([1, 2, 3, 4, 5], 3))  // [1, 2, 3]
}
```


### `list.unfold`

```
list.unfold(seed: a, f: (a) -> Option((b, a))) -> List(b)
```

Builds a list from a seed value. The function returns `Some((element, next_seed))`
to emit an element and continue, or `None` to stop.

```silt
fn main() {
    let countdown = list.unfold(5) { n ->
        when n <= 0 -> None
        else -> Some((n, n - 1))
    }
    println(countdown)  // [5, 4, 3, 2, 1]
}
```


### `list.unique`

```
list.unique(xs: List(a)) -> List(a)
```

Removes duplicate elements, preserving the order of first occurrences.

```silt
fn main() {
    println(list.unique([1, 2, 1, 3, 2]))  // [1, 2, 3]
}
```


### `list.zip`

```
list.zip(xs: List(a), ys: List(b)) -> List((a, b))
```

Pairs up elements from two lists. Stops at the shorter list.

```silt
fn main() {
    let pairs = list.zip([1, 2, 3], ["a", "b", "c"])
    println(pairs)  // [(1, "a"), (2, "b"), (3, "c")]
}
```


## string

Functions for working with immutable strings. Strings use `"..."` literal syntax
with `{expr}` interpolation.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `char_code` | `(String) -> Int` | Unicode code point of first character |
| `chars` | `(String) -> List(String)` | Split string into single-character strings |
| `contains` | `(String, String) -> Bool` | Check if substring exists |
| `ends_with` | `(String, String) -> Bool` | Check suffix |
| `from_char_code` | `(Int) -> String` | Character from Unicode code point |
| `index_of` | `(String, String) -> Option(Int)` | Byte position of first occurrence |
| `byte_length` | `(String) -> Int` | Length in bytes |
| `is_alnum` | `(String) -> Bool` | All chars are alphanumeric |
| `is_alpha` | `(String) -> Bool` | All chars are alphabetic |
| `is_digit` | `(String) -> Bool` | All chars are ASCII digits |
| `is_empty` | `(String) -> Bool` | String has zero length |
| `is_lower` | `(String) -> Bool` | All chars are lowercase |
| `is_upper` | `(String) -> Bool` | All chars are uppercase |
| `is_whitespace` | `(String) -> Bool` | All chars are whitespace |
| `join` | `(List(String), String) -> String` | Join list with separator |
| `length` | `(String) -> Int` | Length in characters |
| `pad_left` | `(String, Int, String) -> String` | Pad to width on the left |
| `pad_right` | `(String, Int, String) -> String` | Pad to width on the right |
| `repeat` | `(String, Int) -> String` | Repeat string n times |
| `replace` | `(String, String, String) -> String` | Replace all occurrences |
| `slice` | `(String, Int, Int) -> String` | Substring by character indices |
| `split` | `(String, String) -> List(String)` | Split on separator |
| `starts_with` | `(String, String) -> Bool` | Check prefix |
| `to_lower` | `(String) -> String` | Convert to lowercase |
| `to_upper` | `(String) -> String` | Convert to uppercase |
| `trim` | `(String) -> String` | Remove leading and trailing whitespace |
| `trim_end` | `(String) -> String` | Remove trailing whitespace |
| `trim_start` | `(String) -> String` | Remove leading whitespace |


### `string.char_code`

```
string.char_code(s: String) -> Int
```

Returns the Unicode code point of the first character. Panics on empty strings.

```silt
fn main() {
    println(string.char_code("A"))  // 65
}
```


### `string.chars`

```
string.chars(s: String) -> List(String)
```

Splits the string into a list of single-character strings.

```silt
fn main() {
    println(string.chars("hi"))  // ["h", "i"]
}
```


### `string.contains`

```
string.contains(s: String, sub: String) -> Bool
```

Returns `true` if `sub` appears anywhere in `s`.

```silt
fn main() {
    println(string.contains("hello world", "world"))  // true
}
```


### `string.ends_with`

```
string.ends_with(s: String, suffix: String) -> Bool
```

Returns `true` if `s` ends with `suffix`.

```silt
fn main() {
    println(string.ends_with("hello.silt", ".silt"))  // true
}
```


### `string.from_char_code`

```
string.from_char_code(code: Int) -> String
```

Converts a Unicode code point to a single-character string. Panics on invalid
code points.

```silt
fn main() {
    println(string.from_char_code(65))  // "A"
}
```


### `string.index_of`

```
string.index_of(s: String, needle: String) -> Option(Int)
```

Returns `Some(byte_index)` of the first occurrence of `needle` in `s`, or
`None` if not found.

```silt
fn main() {
    println(string.index_of("hello", "ll"))  // Some(2)
    println(string.index_of("hello", "z"))   // None
}
```


### `string.is_alnum`

```
string.is_alnum(s: String) -> Bool
```

Returns `true` if all characters are alphanumeric. Returns `false` for empty
strings.

```silt
fn main() {
    println(string.is_alnum("abc123"))  // true
    println(string.is_alnum("abc!"))    // false
    println(string.is_alnum(""))        // false
}
```


### `string.is_alpha`

```
string.is_alpha(s: String) -> Bool
```

Returns `true` if all characters are alphabetic. Returns `false` for empty
strings.

```silt
fn main() {
    println(string.is_alpha("hello"))   // true
    println(string.is_alpha("abc123"))  // false
    println(string.is_alpha(""))        // false
}
```


### `string.is_digit`

```
string.is_digit(s: String) -> Bool
```

Returns `true` if all characters are ASCII digits (0-9). Returns `false`
for empty strings.

```silt
fn main() {
    println(string.is_digit("123"))   // true
    println(string.is_digit("12a"))   // false
    println(string.is_digit(""))      // false
}
```


### `string.is_empty`

```
string.is_empty(s: String) -> Bool
```

Returns `true` if the string has zero length.

```silt
fn main() {
    println(string.is_empty(""))     // true
    println(string.is_empty("hi"))   // false
}
```


### `string.is_lower`

```
string.is_lower(s: String) -> Bool
```

Returns `true` if all characters are lowercase. Returns `false` for empty
strings.

```silt
fn main() {
    println(string.is_lower("hello"))  // true
    println(string.is_lower("Hello"))  // false
    println(string.is_lower(""))       // false
}
```


### `string.is_upper`

```
string.is_upper(s: String) -> Bool
```

Returns `true` if all characters are uppercase. Returns `false` for empty
strings.

```silt
fn main() {
    println(string.is_upper("HELLO"))  // true
    println(string.is_upper("Hello"))  // false
    println(string.is_upper(""))       // false
}
```


### `string.is_whitespace`

```
string.is_whitespace(s: String) -> Bool
```

Returns `true` if all characters are whitespace. Returns `false` for empty
strings.

```silt
fn main() {
    println(string.is_whitespace("  \t"))  // true
    println(string.is_whitespace(" a "))   // false
    println(string.is_whitespace(""))      // false
}
```


### `string.join`

```
string.join(parts: List(String), separator: String) -> String
```

Joins a list of strings with a separator between each pair.

```silt
fn main() {
    let result = string.join(["a", "b", "c"], ", ")
    println(result)  // "a, b, c"
}
```


### `string.byte_length`

```
string.byte_length(s: String) -> Int
```

Returns the length of the string in bytes (UTF-8 encoding). See also
`string.length` which counts characters.

```silt
fn main() {
    println(string.byte_length("hello"))  // 5
    println(string.byte_length("café"))   // 5 (é is 2 bytes)
}
```


### `string.length`

```
string.length(s: String) -> Int
```

Returns the number of characters in the string. Use `string.byte_length` if
you need the size in bytes.

```silt
fn main() {
    println(string.length("hello"))  // 5
    println(string.length("café"))   // 4
}
```


### `string.pad_left`

```
string.pad_left(s: String, width: Int, pad: String) -> String
```

Pads `s` on the left with the first character of `pad` until it reaches
`width`. Returns `s` unchanged if already at or beyond `width`.

```silt
fn main() {
    println(string.pad_left("42", 5, "0"))  // "00042"
}
```


### `string.pad_right`

```
string.pad_right(s: String, width: Int, pad: String) -> String
```

Pads `s` on the right with the first character of `pad` until it reaches
`width`. Returns `s` unchanged if already at or beyond `width`.

```silt
fn main() {
    println(string.pad_right("hi", 5, "."))  // "hi..."
}
```


### `string.repeat`

```
string.repeat(s: String, n: Int) -> String
```

Returns the string repeated `n` times. `n` must be non-negative.

```silt
fn main() {
    println(string.repeat("ab", 3))  // "ababab"
}
```


### `string.replace`

```
string.replace(s: String, from: String, to: String) -> String
```

Replaces all occurrences of `from` with `to`.

```silt
fn main() {
    println(string.replace("hello world", "world", "silt"))
    // "hello silt"
}
```


### `string.slice`

```
string.slice(s: String, start: Int, end: Int) -> String
```

Returns the substring from character index `start` (inclusive) to `end`
(exclusive). Indices are clamped to the string length. Returns an empty string
if `start > end`. Negative indices are a runtime error.

```silt
fn main() {
    println(string.slice("hello", 1, 4))  // "ell"
}
```


### `string.split`

```
string.split(s: String, separator: String) -> List(String)
```

Splits the string on every occurrence of `separator`.

```silt
fn main() {
    let parts = string.split("a,b,c", ",")
    println(parts)  // ["a", "b", "c"]
}
```


### `string.starts_with`

```
string.starts_with(s: String, prefix: String) -> Bool
```

Returns `true` if `s` starts with `prefix`.

```silt
fn main() {
    println(string.starts_with("hello", "hel"))  // true
}
```


### `string.to_lower`

```
string.to_lower(s: String) -> String
```

Converts all characters to lowercase.

```silt
fn main() {
    println(string.to_lower("HELLO"))  // "hello"
}
```


### `string.to_upper`

```
string.to_upper(s: String) -> String
```

Converts all characters to uppercase.

```silt
fn main() {
    println(string.to_upper("hello"))  // "HELLO"
}
```


### `string.trim`

```
string.trim(s: String) -> String
```

Removes leading and trailing whitespace.

```silt
fn main() {
    println(string.trim("  hello  "))  // "hello"
}
```


### `string.trim_end`

```
string.trim_end(s: String) -> String
```

Removes trailing whitespace only.

```silt
fn main() {
    println(string.trim_end("hello   "))  // "hello"
}
```


### `string.trim_start`

```
string.trim_start(s: String) -> String
```

Removes leading whitespace only.

```silt
fn main() {
    println(string.trim_start("   hello"))  // "hello"
}
```


## map

Functions for working with immutable, ordered maps (`Map(k, v)`). Maps use
`#{key: value}` literal syntax. Keys must satisfy the `Hash` trait constraint.

### Summary

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


### `map.contains`

```
map.contains(m: Map(k, v), key: k) -> Bool
```

Returns `true` if the map has an entry for `key`.

```silt
fn main() {
    let m = #{"a": 1, "b": 2}
    println(map.contains(m, "a"))  // true
    println(map.contains(m, "z"))  // false
}
```


### `map.delete`

```
map.delete(m: Map(k, v), key: k) -> Map(k, v)
```

Returns a new map with `key` removed. No-op if key does not exist.

```silt
fn main() {
    let m = #{"a": 1, "b": 2}
    let m2 = map.delete(m, "a")
    println(map.length(m2))  // 1
}
```


### `map.each`

```
map.each(m: Map(k, v), f: (k, v) -> ()) -> ()
```

Calls `f` with each key-value pair. Used for side effects.

```silt
fn main() {
    let m = #{"x": 10, "y": 20}
    map.each(m) { k, v -> println("{k} = {v}") }
}
```


### `map.entries`

```
map.entries(m: Map(k, v)) -> List((k, v))
```

Returns all key-value pairs as a list of tuples.

```silt
fn main() {
    let m = #{"a": 1, "b": 2}
    let pairs = map.entries(m)
    // [("a", 1), ("b", 2)]
}
```


### `map.filter`

```
map.filter(m: Map(k, v), f: (k, v) -> Bool) -> Map(k, v)
```

Returns a new map containing only entries where `f` returns `true`.

```silt
fn main() {
    let m = #{"a": 1, "b": 2, "c": 3}
    let big = map.filter(m) { k, v -> v > 1 }
    // #{"b": 2, "c": 3}
}
```


### `map.from_entries`

```
map.from_entries(entries: List((k, v))) -> Map(k, v)
```

Builds a map from a list of `(key, value)` tuples. Later entries overwrite
earlier ones with the same key.

```silt
fn main() {
    let m = map.from_entries([("a", 1), ("b", 2)])
    println(m)  // #{"a": 1, "b": 2}
}
```


### `map.get`

```
map.get(m: Map(k, v), key: k) -> Option(v)
```

Returns `Some(value)` if the key exists, or `None` otherwise.

```silt
fn main() {
    let m = #{"name": "silt"}
    match map.get(m, "name") {
        Some(v) -> println(v)
        None -> println("not found")
    }
}
```


### `map.keys`

```
map.keys(m: Map(k, v)) -> List(k)
```

Returns all keys as a list, in sorted order.

```silt
fn main() {
    let ks = map.keys(#{"b": 2, "a": 1})
    println(ks)  // ["a", "b"]
}
```


### `map.length`

```
map.length(m: Map(k, v)) -> Int
```

Returns the number of entries in the map.

```silt
fn main() {
    println(map.length(#{"a": 1, "b": 2}))  // 2
}
```


### `map.map`

```
map.map(m: Map(k, v), f: (k, v) -> (k2, v2)) -> Map(k2, v2)
```

Transforms each entry. The callback must return a `(key, value)` tuple.

```silt
fn main() {
    let m = #{"a": 1, "b": 2}
    let doubled = map.map(m) { k, v -> (k, v * 2) }
    // #{"a": 2, "b": 4}
}
```


### `map.merge`

```
map.merge(m1: Map(k, v), m2: Map(k, v)) -> Map(k, v)
```

Merges two maps. When both have the same key, the value from `m2` wins.

```silt
fn main() {
    let a = #{"x": 1, "y": 2}
    let b = #{"y": 99, "z": 3}
    let merged = map.merge(a, b)
    // #{"x": 1, "y": 99, "z": 3}
}
```


### `map.set`

```
map.set(m: Map(k, v), key: k, value: v) -> Map(k, v)
```

Returns a new map with the key set to value. Inserts if new, overwrites if
existing.

```silt
fn main() {
    let m = #{"a": 1}
    let m2 = map.set(m, "b", 2)
    println(m2)  // #{"a": 1, "b": 2}
}
```


### `map.update`

```
map.update(m: Map(k, v), key: k, default: v, f: (v) -> v) -> Map(k, v)
```

If `key` exists, applies `f` to the current value. If `key` does not exist,
applies `f` to `default`. Inserts the result.

```silt
fn main() {
    let m = #{"a": 1}
    let m2 = map.update(m, "a", 0) { v -> v + 10 }
    let m3 = map.update(m2, "b", 0) { v -> v + 10 }
    // m2 == #{"a": 11}
    // m3 == #{"a": 11, "b": 10}
}
```


### `map.values`

```
map.values(m: Map(k, v)) -> List(v)
```

Returns all values as a list, in key-sorted order.

```silt
fn main() {
    let vs = map.values(#{"a": 1, "b": 2})
    println(vs)  // [1, 2]
}
```


## set

Functions for working with immutable, ordered sets (`Set(a)`). Sets use `#[...]`
literal syntax and contain unique values.

### Summary

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


### `set.contains`

```
set.contains(s: Set(a), elem: a) -> Bool
```

Returns `true` if `elem` is in the set.

```silt
fn main() {
    let s = #[1, 2, 3]
    println(set.contains(s, 2))  // true
    println(set.contains(s, 5))  // false
}
```


### `set.difference`

```
set.difference(a: Set(a), b: Set(a)) -> Set(a)
```

Returns elements that are in `a` but not in `b`.

```silt
fn main() {
    let result = set.difference(#[1, 2, 3], #[2, 3, 4])
    println(set.to_list(result))  // [1]
}
```


### `set.each`

```
set.each(s: Set(a), f: (a) -> ()) -> ()
```

Calls `f` for every element. Used for side effects.

```silt
fn main() {
    set.each(#[1, 2, 3]) { x -> println(x) }
}
```


### `set.filter`

```
set.filter(s: Set(a), f: (a) -> Bool) -> Set(a)
```

Returns a new set containing only elements for which `f` returns `true`.

```silt
fn main() {
    let evens = set.filter(#[1, 2, 3, 4]) { x -> x % 2 == 0 }
    println(set.to_list(evens))  // [2, 4]
}
```


### `set.fold`

```
set.fold(s: Set(a), init: b, f: (b, a) -> b) -> b
```

Reduces the set to a single value. Iteration order is sorted.

```silt
fn main() {
    let sum = set.fold(#[1, 2, 3], 0) { acc, x -> acc + x }
    println(sum)  // 6
}
```


### `set.from_list`

```
set.from_list(xs: List(a)) -> Set(a)
```

Creates a set from a list, removing duplicates.

```silt
fn main() {
    let s = set.from_list([1, 2, 2, 3])
    println(set.length(s))  // 3
}
```


### `set.insert`

```
set.insert(s: Set(a), elem: a) -> Set(a)
```

Returns a new set with `elem` added. No-op if already present.

```silt
fn main() {
    let s = set.insert(#[1, 2], 3)
    println(set.to_list(s))  // [1, 2, 3]
}
```


### `set.intersection`

```
set.intersection(a: Set(a), b: Set(a)) -> Set(a)
```

Returns elements that are in both `a` and `b`.

```silt
fn main() {
    let result = set.intersection(#[1, 2, 3], #[2, 3, 4])
    println(set.to_list(result))  // [2, 3]
}
```


### `set.is_subset`

```
set.is_subset(a: Set(a), b: Set(a)) -> Bool
```

Returns `true` if every element of `a` is also in `b`.

```silt
fn main() {
    println(set.is_subset(#[1, 2], #[1, 2, 3]))  // true
    println(set.is_subset(#[1, 4], #[1, 2, 3]))  // false
}
```


### `set.length`

```
set.length(s: Set(a)) -> Int
```

Returns the number of elements in the set.

```silt
fn main() {
    println(set.length(#[1, 2, 3]))  // 3
}
```


### `set.map`

```
set.map(s: Set(a), f: (a) -> b) -> Set(b)
```

Returns a new set with `f` applied to each element. The result set may be
smaller if `f` maps distinct elements to the same value.

```silt
fn main() {
    let result = set.map(#[1, 2, 3]) { x -> x * 10 }
    println(set.to_list(result))  // [10, 20, 30]
}
```


### `set.new`

```
set.new() -> Set(a)
```

Creates a new empty set.

```silt
fn main() {
    let s = set.new()
    let s = set.insert(s, 42)
    println(set.length(s))  // 1
}
```


### `set.remove`

```
set.remove(s: Set(a), elem: a) -> Set(a)
```

Returns a new set with `elem` removed. No-op if not present.

```silt
fn main() {
    let s = set.remove(#[1, 2, 3], 2)
    println(set.to_list(s))  // [1, 3]
}
```


### `set.to_list`

```
set.to_list(s: Set(a)) -> List(a)
```

Converts the set to a sorted list.

```silt
fn main() {
    let xs = set.to_list(#[3, 1, 2])
    println(xs)  // [1, 2, 3]
}
```


### `set.union`

```
set.union(a: Set(a), b: Set(a)) -> Set(a)
```

Returns a set containing all elements from both `a` and `b`.

```silt
fn main() {
    let result = set.union(#[1, 2], #[2, 3])
    println(set.to_list(result))  // [1, 2, 3]
}
```


## int

Functions for parsing, converting, and comparing integers.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `abs` | `(Int) -> Int` | Absolute value |
| `max` | `(Int, Int) -> Int` | Larger of two values |
| `min` | `(Int, Int) -> Int` | Smaller of two values |
| `parse` | `(String) -> Result(Int, String)` | Parse string to integer |
| `to_float` | `(Int) -> Float` | Convert to float |
| `to_string` | `(Int) -> String` | Convert to string |


### `int.abs`

```
int.abs(n: Int) -> Int
```

Returns the absolute value. Runtime error if `n` is `Int` minimum
(`-9223372036854775808`) since the result cannot be represented.

```silt
fn main() {
    println(int.abs(-42))  // 42
    println(int.abs(7))    // 7
}
```


### `int.max`

```
int.max(a: Int, b: Int) -> Int
```

Returns the larger of two integers.

```silt
fn main() {
    println(int.max(3, 7))  // 7
}
```


### `int.min`

```
int.min(a: Int, b: Int) -> Int
```

Returns the smaller of two integers.

```silt
fn main() {
    println(int.min(3, 7))  // 3
}
```


### `int.parse`

```
int.parse(s: String) -> Result(Int, String)
```

Parses a string as an integer. Leading/trailing whitespace is trimmed. Returns
`Ok(n)` on success, `Err(message)` on failure.

```silt
fn main() {
    match int.parse("42") {
        Ok(n) -> println(n)
        Err(e) -> println("parse error: {e}")
    }
}
```


### `int.to_float`

```
int.to_float(n: Int) -> Float
```

Converts an integer to a float.

```silt
fn main() {
    let f = int.to_float(42)
    println(f)  // 42.0
}
```


### `int.to_string`

```
int.to_string(n: Int) -> String
```

Converts an integer to its string representation.

```silt
fn main() {
    let s = int.to_string(42)
    println(s)  // "42"
}
```


## float

Functions for parsing, rounding, converting, and comparing floats.

> **Finite-only floats:** All Float values in Silt are guaranteed to be finite. Operations that would produce NaN or Infinity (such as overflow or invalid math operations) return runtime errors instead. This means floats can be safely compared, sorted, and used as map keys.

> **Note:** `round`, `ceil`, and `floor` return `Float`, not `Int`. Use
> `float.to_int` to convert the result to an integer.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `abs` | `(Float) -> Float` | Absolute value |
| `ceil` | `(Float) -> Float` | Round up to nearest integer (as Float) |
| `floor` | `(Float) -> Float` | Round down to nearest integer (as Float) |
| `max` | `(Float, Float) -> Float` | Larger of two values |
| `min` | `(Float, Float) -> Float` | Smaller of two values |
| `parse` | `(String) -> Result(Float, String)` | Parse string to float |
| `round` | `(Float) -> Float` | Round to nearest integer (as Float) |
| `to_int` | `(Float) -> Int` | Truncate to integer |
| `to_string` | `(Float, Int) -> String` | Format with decimal places |


### `float.abs`

```
float.abs(f: Float) -> Float
```

Returns the absolute value.

```silt
fn main() {
    println(float.abs(-3.14))  // 3.14
}
```


### `float.ceil`

```
float.ceil(f: Float) -> Float
```

Rounds up to the nearest integer, returned as a Float.

```silt
fn main() {
    println(float.ceil(3.2))   // 4.0
    println(float.ceil(-3.2))  // -3.0
}
```


### `float.floor`

```
float.floor(f: Float) -> Float
```

Rounds down to the nearest integer, returned as a Float.

```silt
fn main() {
    println(float.floor(3.9))   // 3.0
    println(float.floor(-3.2))  // -4.0
}
```


### `float.max`

```
float.max(a: Float, b: Float) -> Float
```

Returns the larger of two floats.

```silt
fn main() {
    println(float.max(1.5, 2.5))  // 2.5
}
```


### `float.min`

```
float.min(a: Float, b: Float) -> Float
```

Returns the smaller of two floats.

```silt
fn main() {
    println(float.min(1.5, 2.5))  // 1.5
}
```


### `float.parse`

```
float.parse(s: String) -> Result(Float, String)
```

Parses a string as a float. Leading/trailing whitespace is trimmed. Returns
`Ok(f)` on success, `Err(message)` on failure. Strings like `"NaN"` and
`"Infinity"` are rejected.

```silt
fn main() {
    match float.parse("3.14") {
        Ok(f) -> println(f)
        Err(e) -> println("error: {e}")
    }
}
```


### `float.round`

```
float.round(f: Float) -> Float
```

Rounds to the nearest integer, returned as a Float. Ties round away from zero.

```silt
fn main() {
    println(float.round(3.6))  // 4.0
    println(float.round(3.4))  // 3.0
}
```


### `float.to_int`

```
float.to_int(f: Float) -> Int
```

Truncates toward zero, converting to an integer. Returns a runtime error if
the float is NaN or Infinity.

```silt
fn main() {
    println(float.to_int(3.9))   // 3
    println(float.to_int(-3.9))  // -3
}
```


### `float.to_string`

```
float.to_string(f: Float, decimals: Int) -> String
```

Formats a float as a string with exactly `decimals` decimal places. The
`decimals` argument is required and must be non-negative.

```silt
fn main() {
    println(float.to_string(3.14159, 2))  // "3.14"
    println(float.to_string(42.0, 0))     // "42"
}
```


## result

Functions for transforming and querying `Result(a, e)` values without pattern
matching.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `flat_map` | `(Result(a, e), (a) -> Result(b, e)) -> Result(b, e)` | Chain fallible operations |
| `flatten` | `(Result(Result(a, e), e)) -> Result(a, e)` | Remove one nesting level |
| `is_err` | `(Result(a, e)) -> Bool` | True if Err |
| `is_ok` | `(Result(a, e)) -> Bool` | True if Ok |
| `map_err` | `(Result(a, e), (e) -> f) -> Result(a, f)` | Transform the error |
| `map_ok` | `(Result(a, e), (a) -> b) -> Result(b, e)` | Transform the success value |
| `unwrap_or` | `(Result(a, e), a) -> a` | Extract value or use default |


### `result.flat_map`

```
result.flat_map(r: Result(a, e), f: (a) -> Result(b, e)) -> Result(b, e)
```

If `r` is `Ok(v)`, calls `f(v)` and returns its result. If `r` is `Err`,
returns the `Err` unchanged. Useful for chaining fallible operations.

```silt
fn main() {
    let r = Ok("42")
        |> result.flat_map { s -> int.parse(s) }
    println(r)  // Ok(42)
}
```


### `result.flatten`

```
result.flatten(r: Result(Result(a, e), e)) -> Result(a, e)
```

Collapses a nested Result. `Ok(Ok(v))` becomes `Ok(v)`, `Ok(Err(e))` becomes
`Err(e)`, and `Err(e)` stays `Err(e)`.

```silt
fn main() {
    println(result.flatten(Ok(Ok(42))))         // Ok(42)
    println(result.flatten(Ok(Err("oops"))))    // Err("oops")
}
```


### `result.is_err`

```
result.is_err(r: Result(a, e)) -> Bool
```

Returns `true` if the result is an `Err`.

```silt
fn main() {
    println(result.is_err(Err("fail")))  // true
    println(result.is_err(Ok(42)))       // false
}
```


### `result.is_ok`

```
result.is_ok(r: Result(a, e)) -> Bool
```

Returns `true` if the result is an `Ok`.

```silt
fn main() {
    println(result.is_ok(Ok(42)))       // true
    println(result.is_ok(Err("fail")))  // false
}
```


### `result.map_err`

```
result.map_err(r: Result(a, e), f: (e) -> f) -> Result(a, f)
```

If `r` is `Err(e)`, returns `Err(f(e))`. If `r` is `Ok`, returns it unchanged.

```silt
fn main() {
    let r = Err("not found") |> result.map_err { e -> "Error: {e}" }
    println(r)  // Err("Error: not found")
}
```


### `result.map_ok`

```
result.map_ok(r: Result(a, e), f: (a) -> b) -> Result(b, e)
```

If `r` is `Ok(v)`, returns `Ok(f(v))`. If `r` is `Err`, returns it unchanged.

```silt
fn main() {
    let r = Ok(21) |> result.map_ok { n -> n * 2 }
    println(r)  // Ok(42)
}
```


### `result.unwrap_or`

```
result.unwrap_or(r: Result(a, e), default: a) -> a
```

Returns the `Ok` value, or `default` if the result is `Err`.

```silt
fn main() {
    println(result.unwrap_or(Ok(42), 0))        // 42
    println(result.unwrap_or(Err("fail"), 0))    // 0
}
```


## option

Functions for transforming and querying `Option(a)` values without pattern
matching.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `flat_map` | `(Option(a), (a) -> Option(b)) -> Option(b)` | Chain optional operations |
| `is_none` | `(Option(a)) -> Bool` | True if None |
| `is_some` | `(Option(a)) -> Bool` | True if Some |
| `map` | `(Option(a), (a) -> b) -> Option(b)` | Transform the inner value |
| `to_result` | `(Option(a), e) -> Result(a, e)` | Convert to Result with error value |
| `unwrap_or` | `(Option(a), a) -> a` | Extract value or use default |


### `option.flat_map`

```
option.flat_map(opt: Option(a), f: (a) -> Option(b)) -> Option(b)
```

If `opt` is `Some(v)`, calls `f(v)` and returns its result. If `opt` is `None`,
returns `None`.

```silt
fn main() {
    let result = Some(42) |> option.flat_map { n ->
        when n > 0 -> Some(n * 2)
        else -> None
    }
    println(result)  // Some(84)
}
```


### `option.is_none`

```
option.is_none(opt: Option(a)) -> Bool
```

Returns `true` if the option is `None`.

```silt
fn main() {
    println(option.is_none(None))      // true
    println(option.is_none(Some(1)))   // false
}
```


### `option.is_some`

```
option.is_some(opt: Option(a)) -> Bool
```

Returns `true` if the option is `Some`.

```silt
fn main() {
    println(option.is_some(Some(1)))   // true
    println(option.is_some(None))      // false
}
```


### `option.map`

```
option.map(opt: Option(a), f: (a) -> b) -> Option(b)
```

If `opt` is `Some(v)`, returns `Some(f(v))`. If `opt` is `None`, returns `None`.

```silt
fn main() {
    let result = Some(21) |> option.map { n -> n * 2 }
    println(result)  // Some(42)
}
```


### `option.to_result`

```
option.to_result(opt: Option(a), error: e) -> Result(a, e)
```

Converts `Some(v)` to `Ok(v)` and `None` to `Err(error)`.

```silt
fn main() {
    let r = option.to_result(Some(42), "missing")
    println(r)  // Ok(42)

    let r2 = option.to_result(None, "missing")
    println(r2)  // Err("missing")
}
```


### `option.unwrap_or`

```
option.unwrap_or(opt: Option(a), default: a) -> a
```

Returns the inner value if `Some`, otherwise returns `default`.

```silt
fn main() {
    println(option.unwrap_or(Some(42), 0))  // 42
    println(option.unwrap_or(None, 0))      // 0
}
```


## io

Functions for file I/O, stdin, command-line arguments, and debug inspection.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `args` | `() -> List(String)` | Command-line arguments |
| `inspect` | `(a) -> String` | Debug representation of any value |
| `read_file` | `(String) -> Result(String, String)` | Read entire file as string |
| `read_line` | `() -> Result(String, String)` | Read one line from stdin |
| `write_file` | `(String, String) -> Result((), String)` | Write string to file |


### `io.args`

```
io.args() -> List(String)
```

Returns the command-line arguments as a list of strings, including the program
name.

```silt
fn main() {
    let args = io.args()
    list.each(args) { a -> println(a) }
}
```


### `io.inspect`

```
io.inspect(value: a) -> String
```

Returns a debug-style string representation of any value, using silt syntax
(e.g., strings include quotes, lists show brackets).

```silt
fn main() {
    let s = io.inspect([1, "hello", true])
    println(s)  // [1, "hello", true]
}
```


### `io.read_file`

```
io.read_file(path: String) -> Result(String, String)
```

Reads the entire contents of a file. Returns `Ok(contents)` on success or
`Err(message)` on failure. When called from a spawned task, the operation
transparently yields to the scheduler while the file is being read.

```silt
fn main() {
    match io.read_file("data.txt") {
        Ok(contents) -> println(contents)
        Err(e) -> println("Error: {e}")
    }
}
```


### `io.read_line`

```
io.read_line() -> Result(String, String)
```

Reads a single line from stdin (trailing newline stripped). Returns
`Ok(line)` on success or `Err(message)` on failure. When called from a
spawned task, the operation transparently yields to the scheduler.

```silt
fn main() {
    print("Name: ")
    match io.read_line() {
        Ok(name) -> println("Hello, {name}!")
        Err(e) -> println("Error: {e}")
    }
}
```


### `io.write_file`

```
io.write_file(path: String, contents: String) -> Result((), String)
```

Writes a string to a file, creating or overwriting it. Returns `Ok(())` on
success or `Err(message)` on failure. When called from a spawned task, the
operation transparently yields to the scheduler while the file is being
written.

```silt
fn main() {
    match io.write_file("output.txt", "hello") {
        Ok(_) -> println("written")
        Err(e) -> println("Error: {e}")
    }
}
```


## fs

Filesystem path queries.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `exists` | `(String) -> Bool` | Check if path exists |


### `fs.exists`

```
fs.exists(path: String) -> Bool
```

Returns `true` if the file or directory at `path` exists.

```silt
fn main() {
    when fs.exists("config.toml") -> println("found config")
    else -> println("no config")
}
```


## test

Assertion functions for test scripts. Each accepts an optional trailing `String`
message argument.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `assert` | `(Bool, String?) -> ()` | Assert value is truthy |
| `assert_eq` | `(a, a, String?) -> ()` | Assert two values are equal |
| `assert_ne` | `(a, a, String?) -> ()` | Assert two values are not equal |


### `test.assert`

```
test.assert(condition: Bool) -> ()
test.assert(condition: Bool, message: String) -> ()
```

Panics if `condition` is `false`. The optional message is included in the error.

```silt
fn main() {
    test.assert(1 + 1 == 2)
    test.assert(1 + 1 == 2, "math should work")
}
```


### `test.assert_eq`

```
test.assert_eq(left: a, right: a) -> ()
test.assert_eq(left: a, right: a, message: String) -> ()
```

Panics if `left != right`, displaying both values.

```silt
fn main() {
    test.assert_eq(list.length([1, 2, 3]), 3)
    test.assert_eq(1 + 1, 2, "addition")
}
```


### `test.assert_ne`

```
test.assert_ne(left: a, right: a) -> ()
test.assert_ne(left: a, right: a, message: String) -> ()
```

Panics if `left == right`, displaying both values.

```silt
fn main() {
    test.assert_ne("hello", "world")
}
```


## regex

Regular expression functions. Pattern strings use standard regex syntax.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `captures` | `(String, String) -> Option(List(String))` | Capture groups from first match |
| `captures_all` | `(String, String) -> List(List(String))` | Capture groups from all matches |
| `find` | `(String, String) -> Option(String)` | First match |
| `find_all` | `(String, String) -> List(String)` | All matches |
| `is_match` | `(String, String) -> Bool` | Test if pattern matches |
| `replace` | `(String, String, String) -> String` | Replace first match |
| `replace_all` | `(String, String, String) -> String` | Replace all matches |
| `replace_all_with` | `(String, String, (String) -> String) -> String` | Replace all with callback |
| `split` | `(String, String) -> List(String)` | Split on pattern |


### `regex.captures`

```
regex.captures(pattern: String, text: String) -> Option(List(String))
```

Returns capture groups from the first match, or `None` if no match. The full
match is at index 0, followed by numbered groups.

```silt
fn main() {
    match regex.captures("(\\w+)@(\\w+)", "user@host") {
        Some(groups) -> {
            println(list.get(groups, 1))  // Some("user")
            println(list.get(groups, 2))  // Some("host")
        }
        None -> println("no match")
    }
}
```


### `regex.captures_all`

```
regex.captures_all(pattern: String, text: String) -> List(List(String))
```

Returns capture groups for every match. Each inner list has the full match at
index 0 followed by numbered groups.

```silt
fn main() {
    let results = regex.captures_all("(\\d+)-(\\d+)", "1-2 and 3-4")
    // [["1-2", "1", "2"], ["3-4", "3", "4"]]
}
```


### `regex.find`

```
regex.find(pattern: String, text: String) -> Option(String)
```

Returns `Some(matched_text)` for the first match, or `None`.

```silt
fn main() {
    let result = regex.find("\\d+", "abc 123 def")
    println(result)  // Some("123")
}
```


### `regex.find_all`

```
regex.find_all(pattern: String, text: String) -> List(String)
```

Returns all non-overlapping matches as a list of strings.

```silt
fn main() {
    let nums = regex.find_all("\\d+", "a1 b22 c333")
    println(nums)  // ["1", "22", "333"]
}
```


### `regex.is_match`

```
regex.is_match(pattern: String, text: String) -> Bool
```

Returns `true` if the pattern matches anywhere in the text.

```silt
fn main() {
    println(regex.is_match("^\\d+$", "123"))    // true
    println(regex.is_match("^\\d+$", "abc"))    // false
}
```


### `regex.replace`

```
regex.replace(pattern: String, text: String, replacement: String) -> String
```

Replaces the first match with the replacement string.

```silt
fn main() {
    let result = regex.replace("\\d+", "abc 123 def 456", "NUM")
    println(result)  // "abc NUM def 456"
}
```


### `regex.replace_all`

```
regex.replace_all(pattern: String, text: String, replacement: String) -> String
```

Replaces all matches with the replacement string.

```silt
fn main() {
    let result = regex.replace_all("\\d+", "abc 123 def 456", "NUM")
    println(result)  // "abc NUM def NUM"
}
```


### `regex.replace_all_with`

```
regex.replace_all_with(pattern: String, text: String, f: (String) -> String) -> String
```

Replaces all matches by calling `f` with each matched text. The callback must
return a string.

```silt
fn main() {
    let result = regex.replace_all_with("\\d+", "a1 b22 c333") { m ->
        int.to_string(int.parse(m) |> result.unwrap_or(0) |> fn(n) { n * 2 })
    }
    // "a2 b44 c666"
}
```


### `regex.split`

```
regex.split(pattern: String, text: String) -> List(String)
```

Splits the text on every occurrence of the pattern.

```silt
fn main() {
    let parts = regex.split("\\s+", "hello   world   silt")
    println(parts)  // ["hello", "world", "silt"]
}
```


## json

Parse JSON strings into typed silt values and serialize values to JSON.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `parse` | `(Type, String) -> Result(T, String)` | Parse JSON object into record |
| `parse_list` | `(Type, String) -> Result(List(T), String)` | Parse JSON array into record list |
| `parse_map` | `(Type, String) -> Result(Map(String, v), String)` | Parse JSON object into map |
| `pretty` | `(a) -> String` | Pretty-print value as JSON |
| `stringify` | `(a) -> String` | Serialize value as compact JSON |


### `json.parse`

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
type User {
    name: String,
    age: Int,
}

fn main() {
    let json = "{\"name\": \"Alice\", \"age\": 30}"
    match json.parse(User, json) {
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

fn main() {
    let e = json.parse(Event, "{\"name\": \"launch\", \"date\": \"2024-03-15\"}")?
    println(e.date |> time.weekday)  // Friday
}
```


### `json.parse_list`

```
json.parse_list(T: Type, s: String) -> Result(List(T), String)
```

Parses a JSON array where each element is a record of type `T`.

```silt
type Point {
    x: Int,
    y: Int,
}

fn main() {
    let json = "[{\"x\": 1, \"y\": 2}, {\"x\": 3, \"y\": 4}]"
    match json.parse_list(Point, json) {
        Ok(points) -> list.each(points) { p -> println("{p.x}, {p.y}") }
        Err(e) -> println("Error: {e}")
    }
}
```


### `json.parse_map`

```
json.parse_map(V: Type, s: String) -> Result(Map(String, V), String)
```

Parses a JSON object into a `Map(String, V)`. The first argument is a type
descriptor (`Int`, `Float`, `String`, `Bool`, or a record type).

```silt
fn main() {
    let json = "{\"x\": 10, \"y\": 20}"
    match json.parse_map(Int, json) {
        Ok(m) -> println(map.get(m, "x"))  // Some(10)
        Err(e) -> println("Error: {e}")
    }
}
```


### `json.pretty`

```
json.pretty(value: a) -> String
```

Serializes any value to a pretty-printed JSON string (with indentation and
newlines).

```silt
fn main() {
    let data = #{"name": "silt", "version": 1}
    println(json.pretty(data))
}
```


### `json.stringify`

```
json.stringify(value: a) -> String
```

Serializes any value to a compact JSON string.

```silt
fn main() {
    let data = #{"key": [1, 2, 3]}
    println(json.stringify(data))
    // {"key":[1,2,3]}
}
```


## math

Mathematical functions and constants. All functions operate on `Float` values.

### Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `acos` | `(Float) -> Float` | Arccosine (radians) |
| `asin` | `(Float) -> Float` | Arcsine (radians) |
| `atan` | `(Float) -> Float` | Arctangent (radians) |
| `atan2` | `(Float, Float) -> Float` | Two-argument arctangent |
| `cos` | `(Float) -> Float` | Cosine |
| `e` | `Float` | Euler's number (2.71828...) |
| `log` | `(Float) -> Float` | Natural logarithm (ln) |
| `log10` | `(Float) -> Float` | Base-10 logarithm |
| `pi` | `Float` | Pi (3.14159...) |
| `pow` | `(Float, Float) -> Float` | Exponentiation |
| `sin` | `(Float) -> Float` | Sine |
| `sqrt` | `(Float) -> Float` | Square root |
| `tan` | `(Float) -> Float` | Tangent |


### `math.acos`

```
math.acos(x: Float) -> Float
```

Returns the arccosine of `x` in radians. `x` must be between -1 and 1.

```silt
fn main() {
    println(math.acos(1.0))  // 0.0
}
```


### `math.asin`

```
math.asin(x: Float) -> Float
```

Returns the arcsine of `x` in radians. `x` must be between -1 and 1.

```silt
fn main() {
    println(math.asin(1.0))  // 1.5707... (pi/2)
}
```


### `math.atan`

```
math.atan(x: Float) -> Float
```

Returns the arctangent of `x` in radians.

```silt
fn main() {
    println(math.atan(1.0))  // 0.7853... (pi/4)
}
```


### `math.atan2`

```
math.atan2(y: Float, x: Float) -> Float
```

Returns the angle in radians between the positive x-axis and the point (x, y).
Handles all quadrants correctly.

```silt
fn main() {
    println(math.atan2(1.0, 1.0))  // 0.7853... (pi/4)
}
```


### `math.cos`

```
math.cos(x: Float) -> Float
```

Returns the cosine of `x` (in radians).

```silt
fn main() {
    println(math.cos(0.0))       // 1.0
    println(math.cos(math.pi))   // -1.0
}
```


### `math.e`

```
math.e : Float
```

Euler's number, approximately 2.718281828459045. This is a constant, not a
function.

```silt
fn main() {
    println(math.e)  // 2.718281828459045
}
```


### `math.log`

```
math.log(x: Float) -> Float
```

Returns the natural logarithm (base e) of `x`. `x` must be positive.

```silt
fn main() {
    println(math.log(math.e))  // 1.0
    println(math.log(1.0))     // 0.0
}
```


### `math.log10`

```
math.log10(x: Float) -> Float
```

Returns the base-10 logarithm of `x`. `x` must be positive.

```silt
fn main() {
    println(math.log10(100.0))  // 2.0
}
```


### `math.pi`

```
math.pi : Float
```

Pi, approximately 3.141592653589793. This is a constant, not a function.

```silt
fn main() {
    let circumference = 2.0 * math.pi * 5.0
    println(circumference)
}
```


### `math.pow`

```
math.pow(base: Float, exponent: Float) -> Float
```

Returns `base` raised to the power of `exponent`. Returns a runtime error if
the result would be NaN or Infinity.

```silt
fn main() {
    println(math.pow(2.0, 10.0))  // 1024.0
}
```


### `math.sin`

```
math.sin(x: Float) -> Float
```

Returns the sine of `x` (in radians).

```silt
fn main() {
    println(math.sin(0.0))           // 0.0
    println(math.sin(math.pi / 2.0)) // 1.0
}
```


### `math.sqrt`

```
math.sqrt(x: Float) -> Float
```

Returns the square root of `x`. `x` must be non-negative.

```silt
fn main() {
    println(math.sqrt(4.0))   // 2.0
    println(math.sqrt(2.0))   // 1.4142...
}
```


### `math.tan`

```
math.tan(x: Float) -> Float
```

Returns the tangent of `x` (in radians).

```silt
fn main() {
    println(math.tan(0.0))           // 0.0
    println(math.tan(math.pi / 4.0)) // 1.0 (approximately)
}
```


## channel

Bounded channels for concurrent task communication. Channels provide
communication between tasks spawned with `task.spawn`.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `close` | `(Channel) -> ()` | Close the channel |
| `each` | `(Channel, (a) -> b) -> ()` | Iterate until channel closes |
| `new` | `(Int?) -> Channel` | Create a channel (0 = rendezvous, N = buffered) |
| `receive` | `(Channel) -> ChannelResult(a)` | Blocking receive |
| `select` | `(List(Channel \| (Channel, a))) -> (Channel, ChannelResult(a))` | Wait on multiple operations |
| `send` | `(Channel, a) -> ()` | Blocking send |
| `timeout` | `(Int) -> Channel` | Create a channel that closes after N ms |
| `try_receive` | `(Channel) -> ChannelResult(a)` | Non-blocking receive |
| `try_send` | `(Channel, a) -> Bool` | Non-blocking send |


### `channel.close`

```
channel.close(ch: Channel) -> ()
```

Closes the channel. Subsequent sends will fail. Receivers will see `Closed`
after all buffered messages are consumed.

```silt
fn main() {
    let ch = channel.new(10)
    channel.send(ch, 1)
    channel.close(ch)
}
```


### `channel.each`

```
channel.each(ch: Channel, f: (a) -> b) -> ()
```

Receives messages from the channel and calls `f` with each one, until the
channel is closed. This is the idiomatic way to consume all messages.

```silt
fn main() {
    let ch = channel.new(10)
    task.spawn(fn() {
        channel.send(ch, 1)
        channel.send(ch, 2)
        channel.close(ch)
    })
    channel.each(ch) { msg -> println(msg) }
    // prints 1, then 2
}
```


### `channel.new`

```
channel.new() -> Channel
channel.new(capacity: Int) -> Channel
```

Creates a new channel. With no argument, creates a rendezvous channel
(capacity 0) where the sender blocks until a receiver is ready and vice versa.
With an integer argument, creates a buffered channel with that capacity --
sends block when the buffer is full, receives block when the buffer is empty.

```silt
fn main() {
    let rendezvous = channel.new()    -- true rendezvous (capacity 0)
    let buffered = channel.new(10)    -- buffered (capacity 10)
}
```


### `channel.receive`

```
channel.receive(ch: Channel) -> ChannelResult(a)
```

Receives a value from the channel. Returns `Message(value)` when a value is
available, or `Closed` when the channel is closed and empty. Parks the task
while waiting, allowing other tasks to run on the same thread.

```silt
fn main() {
    let ch = channel.new(1)
    channel.send(ch, 42)
    match channel.receive(ch) {
        Message(v) -> println(v)
        Closed -> println("done")
    }
}
```


### `channel.select`

```
channel.select(ops: List(Channel | (Channel, a))) -> (Channel, ChannelResult(a))
```

Waits until one of the operations completes. Each element of the list is either
a bare channel (receive) or a `(channel, value)` tuple (send). Returns a
2-tuple of `(channel, result)` where `result` is `Message(val)` for a
successful receive, `Sent` for a successful send, or `Closed` if the channel
is closed.

```silt
fn main() {
    let ch1 = channel.new(1)
    let ch2 = channel.new(1)
    task.spawn(fn() { channel.send(ch2, "hello") })
    match channel.select([ch1, ch2]) {
        (^ch2, Message(val)) -> println(val)  // "hello"
        (_, Closed) -> println("closed")
    }
}
```

Send operations in select:

```silt
fn main() {
    let input = channel.new(10)
    let output = channel.new(10)
    channel.send(input, 42)
    match channel.select([input, (output, "result")]) {
        (^input, Message(val)) -> println("received: {val}")
        (^output, Sent) -> println("sent to output")
        (_, Closed) -> println("closed")
    }
}
```


### `channel.send`

```
channel.send(ch: Channel, value: a) -> ()
```

Sends a value into the channel. Parks the task if the buffer is full, allowing
other tasks to run until space opens up.

```silt
fn main() {
    let ch = channel.new(1)
    channel.send(ch, "hello")
}
```


### `channel.timeout`

```
channel.timeout(ms: Int) -> Channel
```

Creates a channel that automatically closes after the given number of
milliseconds. The returned channel carries no values -- it simply closes when
the duration elapses. This is useful for adding deadlines to `channel.select`.

```silt
fn main() {
    let ch = channel.new(10)
    let timer = channel.timeout(1000)  -- closes after 1 second
    match channel.select([ch, timer]) {
        (^ch, Message(val)) -> println("got: {val}")
        (^timer, Closed) -> println("timed out")
    }
}
```


### `channel.try_receive`

```
channel.try_receive(ch: Channel) -> ChannelResult(a)
```

Non-blocking receive. Returns `Message(value)` if a value is immediately
available, `Empty` if the channel is open but has no data, or `Closed` if the
channel is closed and empty.

```silt
fn main() {
    let ch = channel.new(1)
    match channel.try_receive(ch) {
        Message(v) -> println(v)
        Empty -> println("nothing yet")
        Closed -> println("done")
    }
}
```


### `channel.try_send`

```
channel.try_send(ch: Channel, value: a) -> Bool
```

Non-blocking send. Returns `true` if the value was successfully buffered,
`false` if the buffer is full or the channel is closed.

```silt
fn main() {
    let ch = channel.new(1)
    let ok = channel.try_send(ch, 42)
    println(ok)  // true
}
```


## task

Spawn and coordinate lightweight concurrent tasks. Tasks are multiplexed onto a
fixed thread pool and run in parallel. They communicate through channels.

### Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `cancel` | `(Handle) -> ()` | Cancel a running task |
| `join` | `(Handle) -> a` | Wait for a task to complete |
| `spawn` | `(() -> a) -> Handle` | Spawn a new lightweight task |


### `task.cancel`

```
task.cancel(handle: Handle) -> ()
```

Cancels a running task. The task will not execute further. No-op if the task has
already completed.

```silt
fn main() {
    let h = task.spawn(fn() {
        // long-running work
    })
    task.cancel(h)
}
```


### `task.join`

```
task.join(handle: Handle) -> a
```

Blocks until the task completes and returns its result. Parks the calling task
while waiting, allowing other tasks to run.

```silt
fn main() {
    let h = task.spawn(fn() { 1 + 2 })
    let result = task.join(h)
    println(result)  // 3
}
```


### `task.spawn`

```
task.spawn(f: () -> a) -> Handle
```

Spawns a zero-argument function as a lightweight task on the thread pool.
Spawning is cheap -- it allocates a stack, not an OS thread. Returns a handle
that can be used with `task.join` or `task.cancel`.

```silt
fn main() {
    let h = task.spawn(fn() {
        println("running in a task")
        42
    })
    let result = task.join(h)
    println(result)  // 42
}
```


## time

Dates, times, instants, durations, formatting, parsing, and arithmetic. All values are immutable. Nanosecond precision throughout.

### Types

```silt
type Instant  { epoch_ns: Int }                           -- point on the UTC timeline (ns since Unix epoch)
type Date     { year: Int, month: Int, day: Int }          -- calendar date, no time or zone
type Time     { hour: Int, minute: Int, second: Int, ns: Int }  -- wall clock time, no date or zone
type DateTime { date: Date, time: Time }                   -- date + time, no zone
type Duration { ns: Int }                                  -- fixed elapsed time in nanoseconds
type Weekday  { Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday }
```

`Date`, `Time`, and `DateTime` display as ISO 8601 in string interpolation.
`Duration` displays in human-readable form (`2h30m15s`, `500ms`, `42ns`).
Comparison operators (`<`, `>`, `==`) work correctly on all time types.

### Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `now` | `() -> Instant` | Current UTC time as nanosecond epoch |
| `today` | `() -> Date` | Current local date |
| `date` | `(Int, Int, Int) -> Result(Date, String)` | Validated date from year, month, day |
| `time` | `(Int, Int, Int) -> Result(Time, String)` | Validated time from hour, min, sec (ns=0) |
| `datetime` | `(Date, Time) -> DateTime` | Combine date and time (infallible) |
| `to_datetime` | `(Instant, Int) -> DateTime` | Convert instant to local datetime with UTC offset in minutes |
| `to_instant` | `(DateTime, Int) -> Instant` | Convert local datetime to instant with UTC offset in minutes |
| `to_utc` | `(Instant) -> DateTime` | Convert instant to UTC datetime (shorthand for offset=0) |
| `from_utc` | `(DateTime) -> Instant` | Convert UTC datetime to instant (shorthand for offset=0) |
| `format` | `(DateTime, String) -> String` | Format datetime with strftime pattern |
| `format_date` | `(Date, String) -> String` | Format date with strftime pattern |
| `parse` | `(String, String) -> Result(DateTime, String)` | Parse string into datetime with strftime pattern |
| `parse_date` | `(String, String) -> Result(Date, String)` | Parse string into date with strftime pattern |
| `add_days` | `(Date, Int) -> Date` | Add/subtract days from a date |
| `add_months` | `(Date, Int) -> Date` | Add/subtract months, clamping to end-of-month |
| `add` | `(Instant, Duration) -> Instant` | Add duration to an instant |
| `since` | `(Instant, Instant) -> Duration` | Signed duration between two instants (to − from) |
| `hours` | `(Int) -> Duration` | Create duration from hours |
| `minutes` | `(Int) -> Duration` | Create duration from minutes |
| `seconds` | `(Int) -> Duration` | Create duration from seconds |
| `ms` | `(Int) -> Duration` | Create duration from milliseconds |
| `weekday` | `(Date) -> Weekday` | Day of the week |
| `days_between` | `(Date, Date) -> Int` | Signed number of days between two dates |
| `days_in_month` | `(Int, Int) -> Int` | Days in month for given year and month |
| `is_leap_year` | `(Int) -> Bool` | Check if a year is a leap year |
| `sleep` | `(Duration) -> ()` | Fiber-aware sleep |


### `time.now`

```
time.now() -> Instant
```

Returns the current UTC time as nanoseconds since the Unix epoch (1970-01-01T00:00:00Z).

```silt
fn main() {
    let t = time.now()
    println(t.epoch_ns)  // 1775501213453369259
}
```


### `time.today`

```
time.today() -> Date
```

Returns the current date in the system's local timezone.

```silt
fn main() {
    println(time.today())  // 2026-04-06
}
```


### `time.date`

```
time.date(year: Int, month: Int, day: Int) -> Result(Date, String)
```

Creates a validated `Date`. Returns `Err` for invalid dates.

```silt
fn main() {
    println(time.date(2024, 3, 15))   // Ok(2024-03-15)
    println(time.date(2024, 2, 29))   // Ok(2024-02-29) — leap year
    println(time.date(2024, 13, 1))   // Err(invalid date: 2024-13-1)
}
```


### `time.time`

```
time.time(hour: Int, min: Int, sec: Int) -> Result(Time, String)
```

Creates a validated `Time` with `ns` set to 0. Returns `Err` for invalid times.

```silt
fn main() {
    println(time.time(14, 30, 0))  // Ok(14:30:00)
    println(time.time(25, 0, 0))   // Err(invalid time: 25:0:0)
}
```


### `time.datetime`

```
time.datetime(date: Date, time: Time) -> DateTime
```

Combines a `Date` and `Time` into a `DateTime`. Infallible since both inputs are already validated.

```silt
fn main() {
    let d = time.date(2024, 6, 15)?
    let t = time.time(9, 30, 0)?
    println(time.datetime(d, t))  // 2024-06-15T09:30:00
}
```


### `time.to_datetime`

```
time.to_datetime(instant: Instant, offset_minutes: Int) -> DateTime
```

Converts an `Instant` to a `DateTime` by applying a UTC offset in minutes.

```silt
fn main() {
    let now = time.now()
    let tokyo = now |> time.to_datetime(540)    // UTC+9:00
    let india = now |> time.to_datetime(330)    // UTC+5:30
    println(tokyo)
    println(india)
}
```


### `time.to_instant`

```
time.to_instant(datetime: DateTime, offset_minutes: Int) -> Instant
```

Converts a local `DateTime` to an `Instant` by subtracting the UTC offset.

```silt
fn main() {
    let dt = time.datetime(time.date(2024, 1, 1)?, time.time(0, 0, 0)?)
    let instant = time.to_instant(dt, 0)
    println(instant.epoch_ns)
}
```


### `time.to_utc`

```
time.to_utc(instant: Instant) -> DateTime
```

Shorthand for `time.to_datetime(instant, 0)`.

```silt
fn main() {
    println(time.now() |> time.to_utc)  // 2026-04-06T18:46:09.005723612
}
```


### `time.from_utc`

```
time.from_utc(datetime: DateTime) -> Instant
```

Shorthand for `time.to_instant(datetime, 0)`.

```silt
fn main() {
    let dt = time.now() |> time.to_utc
    let back = dt |> time.from_utc
    println(back.epoch_ns)
}
```


### `time.format`

```
time.format(datetime: DateTime, pattern: String) -> String
```

Formats a `DateTime` using strftime patterns. Supported: `%Y %m %d %H %M %S %f %A %a %B %b %%`.

```silt
fn main() {
    let dt = time.datetime(time.date(2024, 12, 25)?, time.time(18, 0, 0)?)
    println(dt |> time.format("%A, %B %d, %Y at %H:%M"))
    // Wednesday, December 25, 2024 at 18:00
}
```


### `time.format_date`

```
time.format_date(date: Date, pattern: String) -> String
```

Formats a `Date` using strftime patterns.

```silt
fn main() {
    let d = time.date(2024, 6, 15)?
    println(d |> time.format_date("%d/%m/%Y"))  // 15/06/2024
}
```


### `time.parse`

```
time.parse(s: String, pattern: String) -> Result(DateTime, String)
```

Parses a string into a `DateTime` using a strftime pattern.

```silt
fn main() {
    let dt = time.parse("2024-07-04 12:00:00", "%Y-%m-%d %H:%M:%S")
    println(dt)  // Ok(2024-07-04T12:00:00)
}
```


### `time.parse_date`

```
time.parse_date(s: String, pattern: String) -> Result(Date, String)
```

Parses a string into a `Date` using a strftime pattern.

```silt
fn main() {
    let d = time.parse_date("2024-07-04", "%Y-%m-%d")
    println(d)  // Ok(2024-07-04)
}
```


### `time.add_days`

```
time.add_days(date: Date, days: Int) -> Date
```

Adds (or subtracts, if negative) days from a date.

```silt
fn main() {
    let d = time.date(2024, 1, 1)?
    println(d |> time.add_days(90))   // 2024-03-31
    println(d |> time.add_days(-1))   // 2023-12-31
}
```


### `time.add_months`

```
time.add_months(date: Date, months: Int) -> Date
```

Adds (or subtracts) months from a date. Clamps to the last valid day of the target month.

```silt
fn main() {
    let d = time.date(2024, 1, 31)?
    println(d |> time.add_months(1))   // 2024-02-29 (leap year, clamped)
    println(d |> time.add_months(2))   // 2024-03-31
}
```


### `time.add`

```
time.add(instant: Instant, duration: Duration) -> Instant
```

Adds a duration to an instant.

```silt
fn main() {
    let t = time.now()
    let later = t |> time.add(time.hours(2))
    println(time.since(t, later))  // 2h
}
```


### `time.since`

```
time.since(from: Instant, to: Instant) -> Duration
```

Returns the signed duration from `from` to `to` (computed as `to.epoch_ns − from.epoch_ns`).

```silt
fn main() {
    let start = time.now()
    time.sleep(time.ms(100))
    let elapsed = time.since(start, time.now())
    println(elapsed)  // 100ms
}
```


### `time.hours`, `time.minutes`, `time.seconds`, `time.ms`

```
time.hours(n: Int) -> Duration
time.minutes(n: Int) -> Duration
time.seconds(n: Int) -> Duration
time.ms(n: Int) -> Duration
```

Duration constructor functions.

```silt
fn main() {
    println(time.hours(1))     // 1h
    println(time.minutes(30))  // 30m
    println(time.seconds(5))   // 5s
    println(time.ms(500))      // 500ms
}
```


### `time.weekday`

```
time.weekday(date: Date) -> Weekday
```

Returns the day of the week. Pattern-match on the result for exhaustive handling.

```silt
fn main() {
    let day = time.today() |> time.weekday
    match day {
        Monday -> println("start of the week")
        Friday -> println("almost weekend")
        Saturday | Sunday -> println("weekend!")
        _ -> println("midweek")
    }
}
```


### `time.days_between`

```
time.days_between(from: Date, to: Date) -> Int
```

Returns the signed number of days between two dates.

```silt
fn main() {
    let a = time.date(2024, 1, 1)?
    let b = time.date(2024, 12, 31)?
    println(time.days_between(a, b))  // 365
}
```


### `time.days_in_month`

```
time.days_in_month(year: Int, month: Int) -> Int
```

Returns the number of days in the given month.

```silt
fn main() {
    println(time.days_in_month(2024, 2))  // 29 (leap year)
    println(time.days_in_month(2023, 2))  // 28
}
```


### `time.is_leap_year`

```
time.is_leap_year(year: Int) -> Bool
```

Returns true if the year is a leap year.

```silt
fn main() {
    println(time.is_leap_year(2024))  // true
    println(time.is_leap_year(1900))  // false (divisible by 100)
    println(time.is_leap_year(2000))  // true (divisible by 400)
}
```


### `time.sleep`

```
time.sleep(duration: Duration) -> ()
```

Blocks the current task for the given duration. Other tasks continue running.

```silt
fn main() {
    let before = time.now()
    time.sleep(time.ms(100))
    let elapsed = time.since(before, time.now())
    println(elapsed)  // ~100ms
}
```


## http

HTTP client and server. Included by default. Exclude with `--no-default-features` for WASM or minimal builds (networking functions will return a runtime error, but `http.segments` still works).

### Types

```silt
type Method { GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS }

type Request {
  method: Method,
  path: String,
  query: String,
  headers: Map(String, String),
  body: String,
}

type Response {
  status: Int,
  body: String,
  headers: Map(String, String),
}
```

`Method` variants are gated constructors -- using `GET`, `POST`, etc. requires `import http`.

### Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `get` | `(String) -> Result(Response, String)` | HTTP GET request |
| `request` | `(Method, String, String, Map(String, String)) -> Result(Response, String)` | HTTP request with method, URL, body, headers |
| `serve` | `(Int, Fn(Request) -> Response) -> ()` | Start a concurrent HTTP server |
| `segments` | `(String) -> List(String)` | Split URL path into segments |


### `http.get`

```
http.get(url: String) -> Result(Response, String)
```

Makes an HTTP GET request. Returns `Ok(Response)` for any successful connection (including 4xx/5xx status codes). Returns `Err(message)` for network errors (DNS failure, connection refused, timeout).

When called from a spawned task, `http.get` transparently yields to the
scheduler while the request is in flight. No API change is needed -- the
call site looks the same.

```silt
fn main() {
  match http.get("https://api.github.com/users/torvalds") {
    Ok(resp) -> println("Status: {resp.status}, body length: {string.length(resp.body)}")
    Err(e) -> println("Network error: {e}")
  }
}
```

Compose with `json.parse` and `?` for typed API responses:

```silt
type User { name: String, id: Int }

fn fetch_user(name) {
  let resp = http.get("https://api.example.com/users/{name}")?
  json.parse(User, resp.body)
}
```


### `http.request`

```
http.request(method: Method, url: String, body: String, headers: Map(String, String)) -> Result(Response, String)
```

Makes an HTTP request with full control over method, body, and headers. Use this for POST, PUT, DELETE, or any request that needs custom headers.

Like `http.get`, this transparently yields to the scheduler when called from
a spawned task.

```silt
-- POST with JSON body
let resp = http.request(
  POST,
  "https://api.example.com/users",
  json.stringify(#{"name": "Alice"}),
  #{"Content-Type": "application/json", "Authorization": "Bearer tok123"}
)?

-- DELETE
let resp = http.request(DELETE, "https://api.example.com/users/42", "", #{})?

-- GET with custom headers
let resp = http.request(GET, "https://api.example.com/data", "", #{"Accept": "text/plain"})?
```


### `http.serve`

```
http.serve(port: Int, handler: Fn(Request) -> Response) -> ()
```

Starts an HTTP server on the given port. Each incoming request is handled on
its own thread with a fresh VM, so multiple requests are processed
concurrently. The accept loop runs on a dedicated OS thread and does not block
the scheduler. If a handler function errors, the server returns a 500 response
without crashing. The handler receives a `Request` and must return a
`Response`. The server runs forever (stop with Ctrl-C).

Use pattern matching on `(req.method, segments)` for routing:

```silt
fn main() {
  println("Listening on :8080")

  http.serve(8080, fn(req) {
    let parts = string.split(req.path, "/")
      |> list.filter { s -> !string.is_empty(s) }

    match (req.method, parts) {
      (GET, []) ->
        Response { status: 200, body: "Hello!", headers: #{} }

      (GET, ["users", id]) ->
        Response { status: 200, body: "User {id}", headers: #{} }

      (POST, ["users"]) ->
        match json.parse(User, req.body) {
          Ok(user) -> Response {
            status: 201,
            body: json.stringify(user),
            headers: #{"Content-Type": "application/json"},
          }
          Err(e) -> Response { status: 400, body: e, headers: #{} }
        }

      _ ->
        Response { status: 404, body: "Not found", headers: #{} }
    }
  })
}
```

Unsupported HTTP methods (e.g. TRACE) receive an automatic 405 response.


### `http.segments`

```
http.segments(path: String) -> List(String)
```

Splits a URL path into non-empty segments. Useful for pattern-matched routing.

```silt
http.segments("/api/users/42")   -- ["api", "users", "42"]
http.segments("/")               -- []
http.segments("//foo//bar/")     -- ["foo", "bar"]
```

This function has no dependencies and works even with `--no-default-features`.
