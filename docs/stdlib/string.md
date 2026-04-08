---
title: "string"
---

# string

Functions for working with immutable strings. Strings use `"..."` literal syntax
with `{expr}` interpolation.

## Summary

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


## `string.char_code`

```
string.char_code(s: String) -> Int
```

Returns the Unicode code point of the first character. Panics on empty strings.

```silt
fn main() {
    println(string.char_code("A"))  // 65
}
```


## `string.chars`

```
string.chars(s: String) -> List(String)
```

Splits the string into a list of single-character strings.

```silt
fn main() {
    println(string.chars("hi"))  // ["h", "i"]
}
```


## `string.contains`

```
string.contains(s: String, sub: String) -> Bool
```

Returns `true` if `sub` appears anywhere in `s`.

```silt
fn main() {
    println(string.contains("hello world", "world"))  // true
}
```


## `string.ends_with`

```
string.ends_with(s: String, suffix: String) -> Bool
```

Returns `true` if `s` ends with `suffix`.

```silt
fn main() {
    println(string.ends_with("hello.silt", ".silt"))  // true
}
```


## `string.from_char_code`

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


## `string.index_of`

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


## `string.is_alnum`

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


## `string.is_alpha`

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


## `string.is_digit`

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


## `string.is_empty`

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


## `string.is_lower`

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


## `string.is_upper`

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


## `string.is_whitespace`

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


## `string.join`

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


## `string.byte_length`

```
string.byte_length(s: String) -> Int
```

Returns the length of the string in bytes (UTF-8 encoding). See also
`string.length` which counts characters.

```silt
fn main() {
    println(string.byte_length("hello"))  // 5
    println(string.byte_length("cafe\u0301"))   // 5 (e\u0301 is 2 bytes)
}
```


## `string.length`

```
string.length(s: String) -> Int
```

Returns the number of characters in the string. Use `string.byte_length` if
you need the size in bytes.

```silt
fn main() {
    println(string.length("hello"))  // 5
    println(string.length("cafe\u0301"))   // 4
}
```


## `string.pad_left`

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


## `string.pad_right`

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


## `string.repeat`

```
string.repeat(s: String, n: Int) -> String
```

Returns the string repeated `n` times. `n` must be non-negative.

```silt
fn main() {
    println(string.repeat("ab", 3))  // "ababab"
}
```


## `string.replace`

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


## `string.slice`

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


## `string.split`

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


## `string.starts_with`

```
string.starts_with(s: String, prefix: String) -> Bool
```

Returns `true` if `s` starts with `prefix`.

```silt
fn main() {
    println(string.starts_with("hello", "hel"))  // true
}
```


## `string.to_lower`

```
string.to_lower(s: String) -> String
```

Converts all characters to lowercase.

```silt
fn main() {
    println(string.to_lower("HELLO"))  // "hello"
}
```


## `string.to_upper`

```
string.to_upper(s: String) -> String
```

Converts all characters to uppercase.

```silt
fn main() {
    println(string.to_upper("hello"))  // "HELLO"
}
```


## `string.trim`

```
string.trim(s: String) -> String
```

Removes leading and trailing whitespace.

```silt
fn main() {
    println(string.trim("  hello  "))  // "hello"
}
```


## `string.trim_end`

```
string.trim_end(s: String) -> String
```

Removes trailing whitespace only.

```silt
fn main() {
    println(string.trim_end("hello   "))  // "hello"
}
```


## `string.trim_start`

```
string.trim_start(s: String) -> String
```

Removes leading whitespace only.

```silt
fn main() {
    println(string.trim_start("   hello"))  // "hello"
}
```
