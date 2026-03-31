# Silt Standard Library Reference

Complete reference for all built-in functions and standard library modules.

Silt has a deliberately small set of **10 global names** that are always available
without any module qualification. Everything else is organized into modules and
accessed with dot notation (e.g. `list.map`, `string.split`, `channel.new`).

-----

## Globals

Always available. No import or qualification needed.

| Name | Kind | Description |
|------|------|-------------|
| `print`  | `print(args...) -> Unit` | Print values separated by spaces, no trailing newline |
| `println` | `println(args...) -> Unit` | Print values separated by spaces, with trailing newline |
| `panic` | `panic(msg) -> !` | Abort execution with an error message |
| `try` | `try(fn) -> Result` | Call a zero-argument function, catching errors into a Result |
| `Ok` | `Ok(value) -> Result` | Construct a success Result |
| `Err` | `Err(value) -> Result` | Construct an error Result |
| `Some` | `Some(value) -> Option` | Construct a present Option |
| `None` | value | The absent Option value (not a function) |
| `Stop` | `Stop(value) -> Step` | Signal early termination from `list.fold_until` |
| `Continue` | `Continue(value) -> Step` | Signal continuation in `list.fold_until` |

These are the **only** names in the global namespace. There is no global `map`,
`filter`, `fold`, `len`, `inspect`, `spawn`, `send`, or `receive`. Use the
module-qualified versions: `list.map`, `list.filter`, `list.fold`,
`list.length`, `io.inspect`, `task.spawn`, `channel.send`, `channel.receive`.

### `print`

```
print(args...) -> Unit
```

Prints all arguments separated by spaces. Does not append a newline.

```silt
fn main() {
  print("hello")
  print(" ")
  print("world")
  -- output: hello world
}
```

### `println`

```
println(args...) -> Unit
```

Prints all arguments separated by spaces, followed by a newline.

```silt
fn main() {
  println("hello, world")
  println("a", "b", "c")
  -- output:
  -- hello, world
  -- a b c
}
```

### `panic`

```
panic(msg) -> !
```

Aborts execution immediately with the given error message. Never returns.

```silt
fn main() {
  panic("something went terribly wrong")
  -- runtime error: panic: something went terribly wrong
}
```

### `try`

```
try(fn) -> Result
```

Calls a zero-argument function and wraps its return value in `Ok`. If the
function causes a runtime error, catches it and returns `Err(message)`.

```silt
fn main() {
  let result = try(fn() { 42 })
  -- Ok(42)

  let result = try(fn() { panic("boom") })
  -- Err("panic: boom")
}
```

### `Ok`

```
Ok(value) -> Result
```

Wraps a value in a successful Result.

```silt
fn main() {
  let r = Ok(42)
  -- Ok(42)
}
```

### `Err`

```
Err(value) -> Result
```

Wraps a value in an error Result.

```silt
fn main() {
  let r = Err("something failed")
  -- Err("something failed")
}
```

### `Some`

```
Some(value) -> Option
```

Wraps a value in a present Option.

```silt
fn main() {
  let opt = Some(42)
  -- Some(42)
}
```

### `None`

```
None -> Option
```

The absent Option value. This is a value, not a function call.

```silt
fn main() {
  let opt = None
  -- None
}
```

-----

## `list` Module

Higher-order functions and utilities for working with lists. Used with the pipe
operator (`|>`) and trailing closures.

| Function | Signature | Description |
|----------|-----------|-------------|
| `list.map` | `list.map(list, fn) -> List` | Apply a function to each element, return new list |
| `list.filter` | `list.filter(list, fn) -> List` | Keep elements where the function returns truthy |
| `list.each` | `list.each(list, fn) -> Unit` | Execute a function for each element (side effects) |
| `list.fold` | `list.fold(list, init, fn) -> T` | Reduce a list to a single value with an accumulator |
| `list.fold_until` | `list.fold_until(list, init, fn) -> T` | Fold with early termination via `Stop(val)` / `Continue(val)` |
| `list.unfold` | `list.unfold(seed, fn) -> List` | Generate a list from a seed; fn returns `Some((elem, next))` or `None` |
| `list.find` | `list.find(list, fn) -> Option` | Return the first element matching the predicate |
| `list.zip` | `list.zip(list_a, list_b) -> List(Tuple)` | Pair up elements from two lists into tuples |
| `list.flatten` | `list.flatten(list) -> List` | Flatten one level of nested lists |
| `list.flat_map` | `list.flat_map(list, fn) -> List` | Map then flatten in one step |
| `list.sort_by` | `list.sort_by(list, key_fn) -> List` | Sort using a key extraction function |
| `list.any` | `list.any(list, fn) -> Bool` | True if any element satisfies the predicate |
| `list.all` | `list.all(list, fn) -> Bool` | True if all elements satisfy the predicate |
| `list.head` | `list.head(list) -> Option` | Get the first element |
| `list.tail` | `list.tail(list) -> List` | Get all elements except the first |
| `list.last` | `list.last(list) -> Option` | Get the last element |
| `list.reverse` | `list.reverse(list) -> List` | Reverse the list |
| `list.sort` | `list.sort(list) -> List` | Sort the list in ascending order |
| `list.contains` | `list.contains(list, value) -> Bool` | Check if list contains a value |
| `list.length` | `list.length(list) -> Int` | Return the number of elements |
| `list.append` | `list.append(list, element) -> List` | Return a new list with the element added at the end |
| `list.concat` | `list.concat(list_a, list_b) -> List` | Concatenate two lists |
| `list.get` | `list.get(list, index) -> Option` | Get element at index (zero-based) |
| `list.take` | `list.take(list, n) -> List` | Take the first n elements |
| `list.drop` | `list.drop(list, n) -> List` | Drop the first n elements |
| `list.enumerate` | `list.enumerate(list) -> List(Tuple(Int, T))` | Pair each element with its index |

### `list.map`

```
list.map(list, fn) -> List
```

Applies `fn` to each element of `list` and returns a new list of results.

```silt
fn main() {
  [1, 2, 3] |> list.map { x -> x * 2 }
  -- [2, 4, 6]
}
```

### `list.filter`

```
list.filter(list, fn) -> List
```

Returns a new list containing only elements for which `fn` returns a truthy value.

```silt
fn main() {
  [1, 2, 3, 4, 5] |> list.filter { x -> x > 2 }
  -- [3, 4, 5]
}
```

### `list.each`

```
list.each(list, fn) -> Unit
```

Calls `fn` on each element for side effects. Returns `Unit`.

```silt
fn main() {
  ["Alice", "Bob"] |> list.each { name -> println("hello {name}") }
  -- output:
  -- hello Alice
  -- hello Bob
}
```

### `list.fold`

```
list.fold(list, init, fn) -> T
```

Reduces a list to a single value. `fn` receives `(accumulator, element)` on each step.

```silt
fn main() {
  [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
  -- 120
}
```

### `list.fold_until`

```
list.fold_until(list, init, fn) -> T
```

Like `list.fold`, but the callback returns `Continue(acc)` to keep going or `Stop(value)` to terminate early. If the list is exhausted without a `Stop`, returns the last accumulator.

```silt
-- Sum until exceeding a threshold
let total = [10, 20, 30, 40] |> list.fold_until(0) { acc, x ->
  match acc + x > 50 {
    true -> Stop(acc)
    _ -> Continue(acc + x)
  }
}
-- total == 60
```

### `list.unfold`

```
list.unfold(seed, fn) -> List
```

Generate a list from a seed value. The callback receives the current state and returns `Some((element, next_state))` to emit an element and continue, or `None` to stop.

```silt
-- Powers of 2 up to 32
let powers = list.unfold(1) { n ->
  match n > 32 {
    true -> None
    _ -> Some((n, n * 2))
  }
}
-- [1, 2, 4, 8, 16, 32]
```

### `list.find`

```
list.find(list, fn) -> Option
```

Returns `Some(element)` for the first element where `fn` returns truthy, or `None` if no match.

```silt
fn main() {
  let result = [1, 2, 3, 4] |> list.find { x -> x > 2 }
  -- Some(3)

  let nothing = [1, 2] |> list.find { x -> x > 10 }
  -- None
}
```

### `list.zip`

```
list.zip(list_a, list_b) -> List(Tuple)
```

Pairs elements from two lists into a list of tuples. Stops at the shorter list.

```silt
fn main() {
  let names = ["Alice", "Bob"]
  let ages = [30, 25]
  list.zip(names, ages)
  -- [("Alice", 30), ("Bob", 25)]
}
```

### `list.flatten`

```
list.flatten(list) -> List
```

Flattens one level of nesting. Non-list elements are kept as-is.

```silt
fn main() {
  [[1, 2], [3, 4], [5]] |> list.flatten
  -- [1, 2, 3, 4, 5]
}
```

### `list.flat_map`

```
list.flat_map(list, fn) -> List
```

Applies `fn` to each element (which should return a list), then flattens the
result one level. Equivalent to `list.map` followed by `list.flatten`.

```silt
fn main() {
  [1, 2, 3] |> list.flat_map { x -> [x, x * 10] }
  -- [1, 10, 2, 20, 3, 30]
}
```

### `list.sort_by`

```
list.sort_by(list, key_fn) -> List
```

Sorts a list using a key extraction function. Elements are compared by the
values returned by `key_fn`.

```silt
fn main() {
  let users = [
    User { name: "Bob", age: 25 },
    User { name: "Alice", age: 30 },
  ]
  users |> list.sort_by { u -> u.age }
  -- sorted by age ascending
}
```

### `list.any`

```
list.any(list, fn) -> Bool
```

Returns `true` if any element satisfies the predicate `fn`.

```silt
fn main() {
  [1, 2, 3] |> list.any { x -> x > 2 }   -- true
  [1, 2, 3] |> list.any { x -> x > 5 }   -- false
}
```

### `list.all`

```
list.all(list, fn) -> Bool
```

Returns `true` if all elements satisfy the predicate `fn`.

```silt
fn main() {
  [2, 4, 6] |> list.all { x -> x % 2 == 0 }   -- true
  [2, 3, 6] |> list.all { x -> x % 2 == 0 }   -- false
}
```

### `list.head`

```
list.head(list) -> Option
```

Returns `Some(element)` for the first element, or `None` if the list is empty.

```silt
fn main() {
  list.head([1, 2, 3])  -- Some(1)
  list.head([])          -- None
}
```

### `list.tail`

```
list.tail(list) -> List
```

Returns a new list with all elements except the first. Returns an empty list if the input is empty.

```silt
fn main() {
  list.tail([1, 2, 3])  -- [2, 3]
  list.tail([])          -- []
}
```

### `list.last`

```
list.last(list) -> Option
```

Returns `Some(element)` for the last element, or `None` if the list is empty.

```silt
fn main() {
  list.last([1, 2, 3])  -- Some(3)
  list.last([])          -- None
}
```

### `list.reverse`

```
list.reverse(list) -> List
```

Returns a new list with elements in reverse order.

```silt
fn main() {
  list.reverse([1, 2, 3])  -- [3, 2, 1]
}
```

### `list.sort`

```
list.sort(list) -> List
```

Returns a new list sorted in ascending order. Uses partial comparison, so elements should be of the same comparable type.

```silt
fn main() {
  list.sort([3, 1, 4, 1, 5])  -- [1, 1, 3, 4, 5]
}
```

### `list.contains`

```
list.contains(list, value) -> Bool
```

Returns `true` if the list contains the given value.

```silt
fn main() {
  list.contains([1, 2, 3], 2)      -- true
  list.contains([1, 2, 3], 99)     -- false
  list.contains(["a", "b"], "a")   -- true
}
```

### `list.length`

```
list.length(list) -> Int
```

Returns the number of elements in the list. This replaces the old bare `len()` function.

```silt
fn main() {
  list.length([1, 2, 3])  -- 3
  list.length([])          -- 0
}
```

### `list.append`

```
list.append(list, element) -> List
```

Returns a new list with `element` added at the end.

```silt
fn main() {
  list.append([1, 2, 3], 4)  -- [1, 2, 3, 4]
}
```

### `list.concat`

```
list.concat(list_a, list_b) -> List
```

Concatenates two lists into a new list.

```silt
fn main() {
  list.concat([1, 2], [3, 4])  -- [1, 2, 3, 4]
}
```

### `list.get`

```
list.get(list, index) -> Option
```

Returns `Some(element)` at the given zero-based index, or `None` if out of bounds.

```silt
fn main() {
  list.get([10, 20, 30], 1)  -- Some(20)
  list.get([10, 20, 30], 5)  -- None
}
```

### `list.take`

```
list.take(list, n) -> List
```

Returns a new list with at most `n` elements from the front.

```silt
fn main() {
  list.take([1, 2, 3, 4, 5], 3)  -- [1, 2, 3]
  list.take([1, 2], 5)            -- [1, 2]
}
```

### `list.drop`

```
list.drop(list, n) -> List
```

Returns a new list with the first `n` elements removed.

```silt
fn main() {
  list.drop([1, 2, 3, 4, 5], 2)  -- [3, 4, 5]
  list.drop([1, 2], 5)            -- []
}
```

### `list.enumerate`

```
list.enumerate(list) -> List(Tuple(Int, T))
```

Returns a list of `(index, element)` tuples.

```silt
fn main() {
  list.enumerate(["a", "b", "c"])
  -- [(0, "a"), (1, "b"), (2, "c")]
}
```

-----

## `string` Module

Functions for working with strings.

| Function | Signature | Description |
|----------|-----------|-------------|
| `string.split` | `string.split(s, sep) -> List(String)` | Split a string by separator |
| `string.join` | `string.join(list, sep) -> String` | Join a list into a string with separator |
| `string.trim` | `string.trim(s) -> String` | Remove leading and trailing whitespace |
| `string.contains` | `string.contains(s, sub) -> Bool` | Check if string contains a substring |
| `string.replace` | `string.replace(s, from, to) -> String` | Replace all occurrences of a substring |
| `string.length` | `string.length(s) -> Int` | Return the byte length of a string |
| `string.to_upper` | `string.to_upper(s) -> String` | Convert to uppercase |
| `string.to_lower` | `string.to_lower(s) -> String` | Convert to lowercase |
| `string.starts_with` | `string.starts_with(s, prefix) -> Bool` | Check if string starts with prefix |
| `string.ends_with` | `string.ends_with(s, suffix) -> Bool` | Check if string ends with suffix |
| `string.chars` | `string.chars(s) -> List(String)` | Split string into single-character strings |
| `string.repeat` | `string.repeat(s, n) -> String` | Repeat a string n times |
| `string.index_of` | `string.index_of(s, needle) -> Option(Int)` | Find index of substring |
| `string.slice` | `string.slice(s, start, end) -> String` | Extract a substring by char indices |
| `string.pad_left` | `string.pad_left(s, width, pad) -> String` | Pad on the left to reach width |
| `string.pad_right` | `string.pad_right(s, width, pad) -> String` | Pad on the right to reach width |

### `string.split`

```
string.split(s, sep) -> List(String)
```

Splits string `s` by the separator `sep`. Returns a list of string parts.

```silt
fn main() {
  "hello world" |> string.split(" ")
  -- ["hello", "world"]

  "a,b,c" |> string.split(",")
  -- ["a", "b", "c"]
}
```

### `string.join`

```
string.join(list, sep) -> String
```

Joins a list of values into a single string, separated by `sep`. Each element is converted to its string representation.

```silt
fn main() {
  string.join(["hello", "world"], " ")
  -- "hello world"

  string.join([1, 2, 3], ", ")
  -- "1, 2, 3"
}
```

### `string.trim`

```
string.trim(s) -> String
```

Removes leading and trailing whitespace from a string.

```silt
fn main() {
  string.trim("  hello  ")
  -- "hello"
}
```

### `string.contains`

```
string.contains(s, sub) -> Bool
```

Returns `true` if `s` contains the substring `sub`.

```silt
fn main() {
  string.contains("hello world", "world")  -- true
  string.contains("hello world", "xyz")    -- false
}
```

### `string.replace`

```
string.replace(s, from, to) -> String
```

Replaces all occurrences of `from` with `to` in string `s`.

```silt
fn main() {
  "host=localhost" |> string.replace("host=", "")
  -- "localhost"
}
```

### `string.length`

```
string.length(s) -> Int
```

Returns the byte length of a string. This replaces the old bare `len()` function for strings.

```silt
fn main() {
  string.length("hello")  -- 5
  string.length("")        -- 0
}
```

### `string.to_upper`

```
string.to_upper(s) -> String
```

Converts all characters in the string to uppercase.

```silt
fn main() {
  string.to_upper("hello")  -- "HELLO"
}
```

### `string.to_lower`

```
string.to_lower(s) -> String
```

Converts all characters in the string to lowercase.

```silt
fn main() {
  string.to_lower("HELLO")  -- "hello"
}
```

### `string.starts_with`

```
string.starts_with(s, prefix) -> Bool
```

Returns `true` if string `s` starts with `prefix`.

```silt
fn main() {
  string.starts_with("hello world", "hello")  -- true
  string.starts_with("hello world", "world")  -- false
}
```

### `string.ends_with`

```
string.ends_with(s, suffix) -> Bool
```

Returns `true` if string `s` ends with `suffix`.

```silt
fn main() {
  string.ends_with("hello world", "world")  -- true
  string.ends_with("hello world", "hello")  -- false
}
```

### `string.chars`

```
string.chars(s) -> List(String)
```

Splits a string into a list of single-character strings.

```silt
fn main() {
  string.chars("abc")
  -- ["a", "b", "c"]
}
```

### `string.repeat`

```
string.repeat(s, n) -> String
```

Repeats a string `n` times. `n` must be a non-negative integer.

```silt
fn main() {
  string.repeat("ha", 3)
  -- "hahaha"

  string.repeat("-", 10)
  -- "----------"
}
```

### `string.index_of`

```
string.index_of(s, needle) -> Option(Int)
```

Returns `Some(index)` of the first occurrence of `needle` in `s`, or `None` if
not found.

```silt
fn main() {
  string.index_of("hello world", "world")  -- Some(6)
  string.index_of("hello world", "xyz")    -- None
}
```

### `string.slice`

```
string.slice(s, start, end) -> String
```

Extracts a substring by character indices (zero-based, exclusive end).

```silt
fn main() {
  string.slice("hello world", 0, 5)   -- "hello"
  string.slice("hello world", 6, 11)  -- "world"
}
```

### `string.pad_left`

```
string.pad_left(s, width, pad_char) -> String
```

Pads a string on the left with `pad_char` until it reaches `width`. If the
string is already at least `width` characters, it is returned unchanged. Only
the first character of `pad_char` is used.

```silt
fn main() {
  string.pad_left("42", 5, "0")    -- "00042"
  string.pad_left("hello", 3, " ") -- "hello" (already wide enough)
}
```

### `string.pad_right`

```
string.pad_right(s, width, pad_char) -> String
```

Pads a string on the right with `pad_char` until it reaches `width`.

```silt
fn main() {
  string.pad_right("42", 5, ".")    -- "42..."
  string.pad_right("hello", 3, " ") -- "hello" (already wide enough)
}
```

-----

## `map` Module

Functions for working with maps (hash maps with string keys). All map operations return new maps (immutable).

| Function | Signature | Description |
|----------|-----------|-------------|
| `map.get` | `map.get(m, key) -> Option` | Look up a key, return `Some(value)` or `None` |
| `map.set` | `map.set(m, key, value) -> Map` | Return a new map with the key set |
| `map.delete` | `map.delete(m, key) -> Map` | Return a new map with the key removed |
| `map.keys` | `map.keys(m) -> List(String)` | Return all keys as a list |
| `map.values` | `map.values(m) -> List` | Return all values as a list |
| `map.merge` | `map.merge(m1, m2) -> Map` | Merge two maps; `m2` values take priority |
| `map.length` | `map.length(m) -> Int` | Return the number of key-value pairs |

### `map.get`

```
map.get(m, key) -> Option
```

Looks up `key` in map `m`. Returns `Some(value)` if found, `None` otherwise.

```silt
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  map.get(m, "name")    -- Some("Alice")
  map.get(m, "email")   -- None
}
```

### `map.set`

```
map.set(m, key, value) -> Map
```

Returns a new map with `key` set to `value`. If the key already exists, its value is replaced.

```silt
fn main() {
  let m = #{ "a": 1 }
  let m2 = map.set(m, "b", 2)
  -- m2 is #{ "a": 1, "b": 2 }
}
```

### `map.delete`

```
map.delete(m, key) -> Map
```

Returns a new map with `key` removed. If the key does not exist, the map is returned unchanged.

```silt
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let m2 = map.delete(m, "a")
  -- m2 is #{ "b": 2 }
}
```

### `map.keys`

```
map.keys(m) -> List(String)
```

Returns a list of all keys in the map.

```silt
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  map.keys(m)
  -- ["age", "name"]  (sorted, BTreeMap order)
}
```

### `map.values`

```
map.values(m) -> List
```

Returns a list of all values in the map.

```silt
fn main() {
  let m = #{ "x": 1, "y": 2 }
  map.values(m)
  -- [1, 2]  (in key-sorted order)
}
```

### `map.merge`

```
map.merge(m1, m2) -> Map
```

Merges two maps. Keys from `m2` override keys from `m1`.

```silt
fn main() {
  let defaults = #{ "host": "localhost", "port": "80" }
  let overrides = #{ "port": "8080" }
  map.merge(defaults, overrides)
  -- #{ "host": "localhost", "port": "8080" }
}
```

### `map.length`

```
map.length(m) -> Int
```

Returns the number of key-value pairs in the map.

```silt
fn main() {
  map.length(#{ "a": 1, "b": 2 })  -- 2
  map.length(#{})                    -- 0
}
```

-----

## `int` Module

Functions for working with integers.

| Function | Signature | Description |
|----------|-----------|-------------|
| `int.parse` | `int.parse(s) -> Result(Int, String)` | Parse a string to an integer |
| `int.abs` | `int.abs(n) -> Int` | Absolute value |
| `int.min` | `int.min(a, b) -> Int` | Return the smaller of two integers |
| `int.max` | `int.max(a, b) -> Int` | Return the larger of two integers |
| `int.to_float` | `int.to_float(n) -> Float` | Convert an integer to a float |
| `int.to_string` | `int.to_string(n) -> String` | Convert an integer to a string |

### `int.parse`

```
int.parse(s) -> Result(Int, String)
```

Parses a string into an integer. Returns `Ok(n)` on success, `Err(message)` on failure. Leading/trailing whitespace is trimmed.

```silt
fn main() {
  int.parse("42")       -- Ok(42)
  int.parse("-7")       -- Ok(-7)
  int.parse("hello")    -- Err("invalid digit found in string")
}
```

### `int.abs`

```
int.abs(n) -> Int
```

Returns the absolute value of an integer.

```silt
fn main() {
  int.abs(-5)   -- 5
  int.abs(3)    -- 3
}
```

### `int.min`

```
int.min(a, b) -> Int
```

Returns the smaller of two integers.

```silt
fn main() {
  int.min(3, 7)    -- 3
  int.min(10, 2)   -- 2
}
```

### `int.max`

```
int.max(a, b) -> Int
```

Returns the larger of two integers.

```silt
fn main() {
  int.max(3, 7)    -- 7
  int.max(10, 2)   -- 10
}
```

### `int.to_float`

```
int.to_float(n) -> Float
```

Converts an integer to a floating-point number.

```silt
fn main() {
  int.to_float(42)   -- 42.0
}
```

### `int.to_string`

```
int.to_string(n) -> String
```

Converts an integer to its string representation.

```silt
fn main() {
  int.to_string(42)    -- "42"
  int.to_string(-7)    -- "-7"
}
```

-----

## `float` Module

Functions for working with floating-point numbers.

| Function | Signature | Description |
|----------|-----------|-------------|
| `float.parse` | `float.parse(s) -> Result(Float, String)` | Parse a string to a float |
| `float.round` | `float.round(f) -> Int` | Round to the nearest integer |
| `float.ceil` | `float.ceil(f) -> Int` | Round up to the nearest integer |
| `float.floor` | `float.floor(f) -> Int` | Round down to the nearest integer |
| `float.abs` | `float.abs(f) -> Float` | Absolute value |
| `float.min` | `float.min(a, b) -> Float` | Return the smaller of two floats |
| `float.max` | `float.max(a, b) -> Float` | Return the larger of two floats |
| `float.to_string` | `float.to_string(f) -> String` | Convert a float to a string |
| `float.to_string` | `float.to_string(f, decimals) -> String` | Format a float to N decimal places |
| `float.to_int` | `float.to_int(f) -> Int` | Truncate a float to an integer |

### `float.parse`

```
float.parse(s) -> Result(Float, String)
```

Parses a string into a float. Returns `Ok(f)` on success, `Err(message)` on failure. Leading/trailing whitespace is trimmed.

```silt
fn main() {
  float.parse("3.14")     -- Ok(3.14)
  float.parse("hello")    -- Err("invalid float literal")
}
```

### `float.round`

```
float.round(f) -> Int
```

Rounds a float to the nearest integer (standard rounding: 0.5 rounds up).

```silt
fn main() {
  float.round(3.7)    -- 4
  float.round(3.2)    -- 3
  float.round(-1.5)   -- -2
}
```

### `float.ceil`

```
float.ceil(f) -> Int
```

Rounds a float up to the nearest integer (toward positive infinity).

```silt
fn main() {
  float.ceil(3.2)    -- 4
  float.ceil(-1.7)   -- -1
}
```

### `float.floor`

```
float.floor(f) -> Int
```

Rounds a float down to the nearest integer (toward negative infinity).

```silt
fn main() {
  float.floor(3.9)    -- 3
  float.floor(-1.2)   -- -2
}
```

### `float.abs`

```
float.abs(f) -> Float
```

Returns the absolute value of a float.

```silt
fn main() {
  float.abs(-3.14)   -- 3.14
  float.abs(2.0)     -- 2.0
}
```

### `float.min`

```
float.min(a, b) -> Float
```

Returns the smaller of two floats.

```silt
fn main() {
  float.min(3.14, 2.72)   -- 2.72
}
```

### `float.max`

```
float.max(a, b) -> Float
```

Returns the larger of two floats.

```silt
fn main() {
  float.max(3.14, 2.72)   -- 3.14
}
```

### `float.to_string`

```
float.to_string(f) -> String
float.to_string(f, decimals) -> String
```

Converts a float to a string. With one argument, uses default formatting. With two arguments, formats to exactly N decimal places (padding with zeros or truncating as needed).

```silt
fn main() {
  float.to_string(3.14)        -- "3.14"
  float.to_string(3.14159, 2)  -- "3.14"
  float.to_string(3.1, 4)      -- "3.1000"
  float.to_string(3.7, 0)      -- "4"
}
```

### `float.to_int`

```
float.to_int(f) -> Int
```

Truncates a float to an integer (rounds toward zero).

```silt
fn main() {
  float.to_int(3.7)    -- 3
  float.to_int(-2.9)   -- -2
}
```

-----

## `result` Module

Functions for working with `Result` values (`Ok(value)` or `Err(error)`).

| Function | Signature | Description |
|----------|-----------|-------------|
| `result.map_ok` | `result.map_ok(result, fn) -> Result` | Transform the Ok value, pass Err through |
| `result.map_err` | `result.map_err(result, fn) -> Result` | Transform the Err value, pass Ok through |
| `result.unwrap_or` | `result.unwrap_or(result, default) -> T` | Extract the Ok value, or return default |
| `result.flatten` | `result.flatten(result) -> Result` | Flatten a nested `Ok(Ok(v))` into `Ok(v)` |
| `result.is_ok` | `result.is_ok(result) -> Bool` | Check if the result is Ok |
| `result.is_err` | `result.is_err(result) -> Bool` | Check if the result is Err |

### `result.map_ok`

```
result.map_ok(result, fn) -> Result
```

If the value is `Ok(v)`, applies `fn` to `v` and wraps the result in `Ok`. If `Err`, passes it through unchanged.

```silt
fn main() {
  result.map_ok(Ok(21), fn(x) { x * 2 })   -- Ok(42)
  result.map_ok(Err("fail"), fn(x) { x })   -- Err("fail")
}
```

### `result.map_err`

```
result.map_err(result, fn) -> Result
```

If the value is `Err(e)`, applies `fn` to `e` and wraps the result in `Err`. If `Ok`, passes it through unchanged.

```silt
fn main() {
  result.map_err(Err("fail"), fn(e) { "error: {e}" })
  -- Err("error: fail")

  result.map_err(Ok(42), fn(e) { "error: {e}" })
  -- Ok(42)
}
```

### `result.unwrap_or`

```
result.unwrap_or(result, default) -> T
```

If the value is `Ok(v)` or `Some(v)`, returns `v`. Otherwise returns `default`.

```silt
fn main() {
  result.unwrap_or(Ok(42), 0)       -- 42
  result.unwrap_or(Err("nope"), 0)  -- 0
}
```

### `result.flatten`

```
result.flatten(result) -> Result
```

Flattens a nested Result. `Ok(Ok(v))` becomes `Ok(v)`, `Ok(Err(e))` becomes `Err(e)`, and `Err(e)` stays `Err(e)`.

```silt
fn main() {
  result.flatten(Ok(Ok(42)))          -- Ok(42)
  result.flatten(Ok(Err("inner")))    -- Err("inner")
  result.flatten(Err("outer"))        -- Err("outer")
}
```

### `result.is_ok`

```
result.is_ok(result) -> Bool
```

Returns `true` if the value is `Ok`, `false` if `Err`.

```silt
fn main() {
  result.is_ok(Ok(1))       -- true
  result.is_ok(Err("no"))   -- false
}
```

### `result.is_err`

```
result.is_err(result) -> Bool
```

Returns `true` if the value is `Err`, `false` if `Ok`.

```silt
fn main() {
  result.is_err(Err("no"))   -- true
  result.is_err(Ok(1))       -- false
}
```

-----

## `option` Module

Functions for working with `Option` values (`Some(value)` or `None`).

| Function | Signature | Description |
|----------|-----------|-------------|
| `option.map` | `option.map(opt, fn) -> Option` | Transform the Some value, pass None through |
| `option.unwrap_or` | `option.unwrap_or(opt, default) -> T` | Extract the inner value, or return default |
| `option.to_result` | `option.to_result(opt, err) -> Result` | Convert Option to Result with an error value |
| `option.is_some` | `option.is_some(opt) -> Bool` | Check if the option is Some |
| `option.is_none` | `option.is_none(opt) -> Bool` | Check if the option is None |

### `option.map`

```
option.map(opt, fn) -> Option
```

If the value is `Some(v)`, applies `fn` to `v` and wraps the result in `Some`. If `None`, returns `None`.

```silt
fn main() {
  option.map(Some(21), fn(x) { x * 2 })   -- Some(42)
  option.map(None, fn(x) { x * 2 })       -- None
}
```

### `option.unwrap_or`

```
option.unwrap_or(opt, default) -> T
```

If the value is `Some(v)`, returns `v`. If `None`, returns `default`.

```silt
fn main() {
  option.unwrap_or(Some(42), 0)   -- 42
  option.unwrap_or(None, 0)       -- 0
}
```

### `option.to_result`

```
option.to_result(opt, err) -> Result
```

Converts an Option to a Result. `Some(v)` becomes `Ok(v)`, `None` becomes `Err(err)`.

```silt
fn main() {
  option.to_result(Some(42), "missing")     -- Ok(42)
  option.to_result(None, "missing")         -- Err("missing")
}
```

### `option.is_some`

```
option.is_some(opt) -> Bool
```

Returns `true` if the value is `Some`, `false` if `None`.

```silt
fn main() {
  option.is_some(Some(1))   -- true
  option.is_some(None)      -- false
}
```

### `option.is_none`

```
option.is_none(opt) -> Bool
```

Returns `true` if the value is `None`, `false` if `Some`.

```silt
fn main() {
  option.is_none(None)      -- true
  option.is_none(Some(1))   -- false
}
```

-----

## `io` Module

Functions for file I/O, standard input, debug inspection, and command-line arguments.

| Function | Signature | Description |
|----------|-----------|-------------|
| `io.inspect` | `io.inspect(value) -> String` | Return the debug representation of a value |
| `io.read_file` | `io.read_file(path) -> Result(String, String)` | Read an entire file as a string |
| `io.write_file` | `io.write_file(path, content) -> Result(Unit, String)` | Write a string to a file |
| `io.read_line` | `io.read_line() -> Result(String, String)` | Read one line from stdin |
| `io.args` | `io.args() -> List(String)` | Get command-line arguments |

### `io.inspect`

```
io.inspect(value) -> String
```

Returns the debug representation of any value as a String. Useful for debugging complex data structures.

```silt
fn main() {
  let xs = [1, 2, 3]
  println(io.inspect(xs))
  -- output: List([Int(1), Int(2), Int(3)])
}
```

### `io.read_file`

```
io.read_file(path) -> Result(String, String)
```

Reads the entire contents of a file as a string. Returns `Ok(content)` on success, `Err(message)` on failure.

```silt
fn main() {
  match io.read_file("data.txt") {
    Ok(content) -> println(content)
    Err(e) -> println("error: {e}")
  }
}
```

### `io.write_file`

```
io.write_file(path, content) -> Result(Unit, String)
```

Writes `content` to a file at `path`, creating or overwriting it. Returns `Ok(())` on success, `Err(message)` on failure.

```silt
fn main() {
  match io.write_file("out.txt", "hello world") {
    Ok(_) -> println("written")
    Err(e) -> println("error: {e}")
  }
}
```

### `io.read_line`

```
io.read_line() -> Result(String, String)
```

Reads one line from standard input. The trailing newline is stripped. Returns `Ok(line)` on success, `Err(message)` on failure.

```silt
fn main() {
  match io.read_line() {
    Ok(name) -> println("hello {name}")
    Err(e) -> println("error: {e}")
  }
}
```

### `io.args`

```
io.args() -> List(String)
```

Returns the command-line arguments as a list of strings. The first element is typically the program name.

```silt
fn main() {
  let args = io.args()
  args |> list.each { a -> println(a) }
}
```

-----

## `test` Module

Assertion functions for testing.

| Function | Signature | Description |
|----------|-----------|-------------|
| `test.assert` | `test.assert(value) -> Unit` | Assert that a value is truthy |
| `test.assert_eq` | `test.assert_eq(a, b) -> Unit` | Assert that two values are equal |
| `test.assert_ne` | `test.assert_ne(a, b) -> Unit` | Assert that two values are not equal |

### `test.assert`

```
test.assert(value) -> Unit
```

Passes if `value` is truthy. Aborts with an error if the value is falsy.

```silt
fn test_basics() {
  test.assert(true)
  test.assert(1 + 1 == 2)
}
```

### `test.assert_eq`

```
test.assert_eq(a, b) -> Unit
```

Passes if `a` equals `b`. Aborts with a message showing both values if they differ.

```silt
fn test_addition() {
  test.assert_eq(1 + 1, 2)
  test.assert_eq("hello", "hello")
}
```

### `test.assert_ne`

```
test.assert_ne(a, b) -> Unit
```

Passes if `a` does not equal `b`. Aborts with a message showing both values if they are equal.

```silt
fn test_not_equal() {
  test.assert_ne(1, 2)
  test.assert_ne("hello", "world")
}
```

-----

## `channel` Module

CSP-style typed channel operations. These have special evaluation semantics for
cooperative scheduling (they receive unevaluated expressions so the scheduler
can manage blocking).

| Function | Signature | Description |
|----------|-----------|-------------|
| `channel.new` | `channel.new() -> Channel` / `channel.new(n) -> Channel` | Create a channel |
| `channel.send` | `channel.send(ch, value) -> Unit` | Send a value into a channel |
| `channel.receive` | `channel.receive(ch) -> T` | Receive a value from a channel |
| `channel.close` | `channel.close(ch) -> Unit` | Close a channel; no more sends allowed |
| `channel.select` | `channel.select(channels) -> (Channel, T)` | Wait on multiple channels; returns `(channel, value)` |
| `channel.try_send` | `channel.try_send(ch, value) -> Bool` | Non-blocking send; true if sent |
| `channel.try_receive` | `channel.try_receive(ch) -> Option` | Non-blocking receive; Some(value) or None |

### `channel.new`

```
channel.new() -> Channel
channel.new(capacity) -> Channel
```

Creates a new channel. With no arguments, creates an unbuffered channel (capacity 0). With a non-negative integer argument, creates a buffered channel with that capacity.

```silt
fn main() {
  let unbuffered = channel.new()
  let buffered = channel.new(10)
}
```

### `channel.send`

```
channel.send(ch, value) -> Unit
```

Sends a value into a channel. If the channel buffer is full, cooperatively yields to other tasks until space is available. Errors with a deadlock message if no progress can be made.

```silt
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.send(ch, "hello")
  channel.send(ch, [1, 2, 3])
}
```

### `channel.receive`

```
channel.receive(ch) -> T
```

Receives a value from a channel. If the channel is empty, cooperatively yields to other tasks until a value is available. Errors with a deadlock message if no progress can be made.

```silt
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  let value = channel.receive(ch)
  println(value)   -- 42
}
```

### `channel.close`

```
channel.close(ch) -> Unit
```

Closes a channel. After closing, `channel.send` on the channel will error. `channel.receive` on a closed channel returns any remaining buffered values; once the buffer is empty, it returns `None`.

```silt
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, "hello")
    channel.send(ch, "world")
    channel.close(ch)
  })

  let consumer = task.spawn(fn() {
    let msg1 = channel.receive(ch)
    let msg2 = channel.receive(ch)
    let msg3 = channel.receive(ch)   -- None (channel closed and empty)
    println("{msg1} {msg2}")
  })

  task.join(producer)
  task.join(consumer)
}
```

### `channel.try_send`

```
channel.try_send(ch, value) -> Bool
```

Attempts a non-blocking send. Returns `true` if the value was successfully sent, `false` if the channel buffer is full or the channel is closed. Never blocks.

```silt
fn main() {
  let ch = channel.new(1)

  let sent1 = channel.try_send(ch, "first")    -- true (buffer has space)
  let sent2 = channel.try_send(ch, "second")   -- false (buffer full)

  println("sent1: {sent1}")   -- true
  println("sent2: {sent2}")   -- false
}
```

### `channel.try_receive`

```
channel.try_receive(ch) -> Option
```

Attempts a non-blocking receive. Returns `Some(value)` if a value is available, `None` if the channel is empty or closed. Never blocks.

```silt
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)

  let got1 = channel.try_receive(ch)   -- Some(42)
  let got2 = channel.try_receive(ch)   -- None (channel empty)

  println("got1: {got1}")   -- Some(42)
  println("got2: {got2}")   -- None
}
```

-----

## `task` Module

Task spawning and lifecycle management.

| Function | Signature | Description |
|----------|-----------|-------------|
| `task.spawn` | `task.spawn(fn) -> Handle` | Spawn a concurrent task |
| `task.join` | `task.join(handle) -> T` | Wait for a task to complete and get its result |
| `task.cancel` | `task.cancel(handle) -> Unit` | Cancel a spawned task |

### `task.spawn`

```
task.spawn(fn) -> Handle
```

Spawns a concurrent task from a zero-argument function. Returns a handle that can be used with `task.join` or `task.cancel`. The task runs cooperatively, interleaved with other tasks.

```silt
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, "hello")
    channel.send(ch, "world")
  })

  task.join(producer)
  let msg1 = channel.receive(ch)
  let msg2 = channel.receive(ch)
  println("{msg1} {msg2}")
}
```

### `task.join`

```
task.join(handle) -> T
```

Waits for a spawned task to complete and returns its result value. Runs pending tasks cooperatively while waiting. Errors if the joined task failed or if a deadlock is detected.

```silt
fn main() {
  let h = task.spawn(fn() { 42 })
  let result = task.join(h)
  println(result)   -- 42
}
```

### `task.cancel`

```
task.cancel(handle) -> Unit
```

Cancels a spawned task. The task will not be scheduled for further execution.

```silt
fn main() {
  let h = task.spawn(fn() { 42 })
  task.cancel(h)
}
```

-----

### `channel.select`

```
channel.select(channels) -> (Channel, T)
```

Waits on multiple channels simultaneously and returns a `(channel, value)` tuple
for whichever channel has data first. The channels are polled in order. If no
channel is ready, the scheduler cooperatively runs pending tasks and retries.
Errors with a deadlock message if no progress can be made.

Use the `^` pin operator in pattern matching to identify which channel produced
the value. The `^` prefix matches against the current value of an existing
variable instead of creating a new binding.

```silt
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  channel.send(ch2, "from ch2")

  match channel.select([ch1, ch2]) {
    (^ch1, msg) -> println("got from ch1: {msg}")
    (^ch2, msg) -> println("got from ch2: {msg}")
    _ -> panic("unexpected")
  }
  -- "got from ch2: from ch2"
}
```
