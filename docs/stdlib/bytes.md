---
title: "bytes"
section: "Standard Library"
order: 18
---

# bytes

Immutable byte sequences. The `Bytes` value type carries arbitrary binary
data — useful for protocol parsing, file I/O, hashing, encoding/decoding,
and network communication: what [`tcp`](tcp.md) reads and writes, and
every body of an [`http`](http.md) request or response.

`Bytes` values use **structural equality**: two byte sequences with the
same content compare equal regardless of how they were constructed. They
work as `Map`/`Set` keys and respect the standard `==` operator.

`Bytes` values are ordered byte by byte, each byte a number from 0 to
255; when one value is the start of the other, the shorter comes
first. `<`, `<=`, `>`, `>=`, `Compare.compare`, `list.sort`,
`list.max` and `list.min` use this order, and so do map keys and set
elements when they are printed or walked.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `concat` | `(Bytes, Bytes) -> Bytes` | Concatenate two byte sequences |
| `concat_all` | `(List(Bytes)) -> Bytes` | Concatenate every element of a list |
| `contains` | `(Bytes, Bytes) -> Bool` | True if `needle` appears in `b` |
| `empty` | `() -> Bytes` | Zero-length byte sequence |
| `ends_with` | `(Bytes, Bytes) -> Bool` | True if `b` ends with `suffix` |
| `from_base64` | `(String) -> Result(Bytes, BytesError)` | Decode base64 string |
| `from_hex` | `(String) -> Result(Bytes, BytesError)` | Decode hex string (case-insensitive) |
| `from_list` | `(List(Int)) -> Result(Bytes, BytesError)` | Build from a list of byte values (0..=255) |
| `from_string` | `(String) -> Bytes` | UTF-8 encode a string |
| `get` | `(Bytes, Int) -> Option(Int)` | The byte at index, or None |
| `index_of` | `(Bytes, Bytes) -> Option(Int)` | First offset at which `needle` appears |
| `is_empty` | `(Bytes) -> Bool` | True if there are no bytes |
| `length` | `(Bytes) -> Int` | Number of bytes |
| `slice` | `(Bytes, Int, Int) -> Bytes` | The bytes from `start` up to `end`, both cut to the bytes there are |
| `split` | `(Bytes, Bytes) -> List(Bytes)` | Split on every occurrence of `sep` (an empty `sep` splits into the single bytes) |
| `starts_with` | `(Bytes, Bytes) -> Bool` | True if `b` begins with `prefix` |
| `to_base64` | `(Bytes) -> String` | Encode as base64 |
| `to_hex` | `(Bytes) -> String` | Encode as lowercase hex |
| `to_list` | `(Bytes) -> List(Int)` | Materialize as a list of byte values |
| `to_string` | `(Bytes) -> Result(String, BytesError)` | UTF-8 decode (errors on invalid UTF-8) |
| `to_string_lossy` | `(Bytes) -> String` | UTF-8 decode, each invalid sequence as U+FFFD |

## `bytes.concat`

```
bytes.concat(a: Bytes, b: Bytes) -> Bytes
```

The bytes of `a` followed by the bytes of `b`.

```silt
import bytes

fn main() {
  let b = bytes.concat(bytes.from_string("ab"), bytes.from_string("cd"))
  println(bytes.to_hex(b)) -- 61626364
}
```


## `bytes.concat_all`

```
bytes.concat_all(parts: List(Bytes)) -> Bytes
```

The bytes of every element of `parts`, in order; an empty list gives empty `Bytes`.

```silt
import bytes

fn main() {
  let b = bytes.concat_all([bytes.from_string("a"), bytes.from_string("b"), bytes.from_string("c")])
  println(bytes.to_string_lossy(b)) -- abc
  println(bytes.length(bytes.concat_all([]))) -- 0
}
```


## `bytes.contains`

```
bytes.contains(b: Bytes, needle: Bytes) -> Bool
```

Whether `needle` appears anywhere in `b`. An empty `needle` is in every value.

```silt
import bytes

fn main() {
  let msg = bytes.from_string("key=value")
  println(bytes.contains(msg, bytes.from_string("="))) -- true
  println(bytes.contains(msg, bytes.from_string("#"))) -- false
}
```


## `bytes.empty`

```
bytes.empty() -> Bytes
```

`Bytes` of length 0.

```silt
import bytes

fn main() {
  println(bytes.length(bytes.empty())) -- 0
  println(bytes.is_empty(bytes.empty())) -- true
}
```


## `bytes.ends_with`

```
bytes.ends_with(b: Bytes, suffix: Bytes) -> Bool
```

Whether `b` ends with `suffix`. An empty `suffix` is the end of every value.

```silt
import bytes

fn main() {
  let file = bytes.from_string("report.csv")
  println(bytes.ends_with(file, bytes.from_string(".csv"))) -- true
  println(bytes.ends_with(file, bytes.from_string(".txt"))) -- false
}
```


## `bytes.from_base64`

```
bytes.from_base64(s: String) -> Result(Bytes, BytesError)
```

Decodes base64 text (the standard alphabet, with its `=` padding). Malformed text, or text without its padding, is `Err(InvalidBase64(msg))`.

```silt
import bytes
import result

fn main() {
  println(bytes.from_base64("aGk=") |> result.map(bytes.to_string_lossy)) -- Ok("hi")
  match bytes.from_base64("aGk") {
    Ok(_) -> println("decoded")
    Err(e) -> println("{e}") -- invalid base64: invalid padding: a base64 text must be padded with `=` to a whole number of groups of 4 characters
  }
}
```


## `bytes.from_hex`

```
bytes.from_hex(s: String) -> Result(Bytes, BytesError)
```

Decodes hex text, two digits a byte, in either case. An odd number of digits, or a character that is no hex digit, is `Err(InvalidHex(msg))`.

```silt
import bytes
import result

fn main() {
  println(bytes.from_hex("DEADbeef") |> result.map(bytes.to_list)) -- Ok([222, 173, 190, 239])
  match bytes.from_hex("abc") {
    Ok(_) -> println("decoded")
    Err(e) -> println("{e}") -- invalid hex: hex string must have even length, got 3 chars
  }
}
```


## `bytes.from_list`

```
bytes.from_list(xs: List(Int)) -> Result(Bytes, BytesError)
```

`Bytes` from a list of byte values. An element below 0 or above 255 is `Err(ByteOutOfRange(value))`.

```silt
import bytes
import result

fn main() {
  println(bytes.from_list([104, 105]) |> result.map(bytes.to_string_lossy)) -- Ok("hi")
  match bytes.from_list([1, 256]) {
    Ok(_) -> println("built")
    Err(e) -> println("{e}") -- byte value out of range (expected 0..=255): 256
  }
}
```


## `bytes.from_string`

```
bytes.from_string(s: String) -> Bytes
```

The UTF-8 encoding of `s`: one byte a character of ASCII, two to four for others.

```silt
import bytes

fn main() {
  println(bytes.length(bytes.from_string("abc"))) -- 3
  println(bytes.to_hex(bytes.from_string("é"))) -- c3a9
}
```


## `bytes.get`

```
bytes.get(b: Bytes, i: Int) -> Option(Int)
```

The byte at `i` (from 0), as an `Int` from 0 to 255, or `None` when there is no byte there.

```silt
import bytes

fn main() {
  let b = bytes.from_string("hi")
  println(bytes.get(b, 1)) -- Some(105)
  println(bytes.get(b, 2)) -- None
}
```


## `bytes.index_of`

```
bytes.index_of(b: Bytes, needle: Bytes) -> Option(Int)
```

The offset of the first place `needle` appears in `b`, or `None`. An empty `needle` is at `Some(0)`.

```silt
import bytes

fn main() {
  let line = bytes.from_string("GET /index.html")
  println(bytes.index_of(line, bytes.from_string(" "))) -- Some(3)
  println(bytes.index_of(line, bytes.from_string("POST"))) -- None
}
```


## `bytes.is_empty`

```
bytes.is_empty(b: Bytes) -> Bool
```

Whether `b` has no bytes.

```silt
import bytes

fn main() {
  println(bytes.is_empty(bytes.from_string(""))) -- true
  println(bytes.is_empty(bytes.from_string("x"))) -- false
}
```


## `bytes.length`

```
bytes.length(b: Bytes) -> Int
```

The number of bytes in `b`.

```silt
import bytes

fn main() {
  println(bytes.length(bytes.from_string("héllo"))) -- 6
}
```


## `bytes.slice`

```
bytes.slice(b: Bytes, start: Int, end: Int) -> Bytes
```

The bytes from `start` up to, not including, `end`. Both are cut to the bytes there are: a negative index counts as 0, one past the end as the end, and `start` after `end` gives empty `Bytes`.

```silt
import bytes

fn main() {
  let b = bytes.from_string("hello world")
  println(bytes.to_string_lossy(bytes.slice(b, 6, 11))) -- world
  println(bytes.to_string_lossy(bytes.slice(b, 6, 99))) -- world
  println(bytes.length(bytes.slice(b, 8, 2))) -- 0
}
```


## `bytes.split`

```
bytes.split(b: Bytes, sep: Bytes) -> List(Bytes)
```

The pieces of `b` between the places `sep` appears, as `string.split` cuts a string; an empty `sep` gives the single bytes.

```silt
import bytes
import list

fn main() {
  let parts = bytes.split(bytes.from_string("a,b,,c"), bytes.from_string(","))
  println(list.map(parts, bytes.to_string_lossy)) -- ["a", "b", "", "c"]
}
```


## `bytes.starts_with`

```
bytes.starts_with(b: Bytes, prefix: Bytes) -> Bool
```

Whether `b` begins with `prefix`. An empty `prefix` is the start of every value.

```silt
import bytes

fn main() {
  let b = bytes.from_string("%PDF-1.7")
  println(bytes.starts_with(b, bytes.from_string("%PDF"))) -- true
}
```


## `bytes.to_base64`

```
bytes.to_base64(b: Bytes) -> String
```

`b` as base64 text: the standard alphabet, with `=` padding.

```silt
import bytes

fn main() {
  println(bytes.to_base64(bytes.from_string("hi"))) -- aGk=
}
```


## `bytes.to_hex`

```
bytes.to_hex(b: Bytes) -> String
```

`b` as hex text, two lowercase digits a byte.

```silt
import bytes

fn main() {
  println(bytes.to_hex(bytes.from_string("hi"))) -- 6869
}
```


## `bytes.to_list`

```
bytes.to_list(b: Bytes) -> List(Int)
```

The bytes of `b` as a list of `Int`s from 0 to 255.

```silt
import bytes

fn main() {
  println(bytes.to_list(bytes.from_string("hi"))) -- [104, 105]
}
```


## `bytes.to_string`

```
bytes.to_string(b: Bytes) -> Result(String, BytesError)
```

Decodes `b` as UTF-8. Bytes that are no UTF-8 are `Err(InvalidUtf8(offset))`, with the offset of the first bad byte.

```silt
import bytes

fn main() {
  println(bytes.to_string(bytes.from_string("ok"))) -- Ok("ok")
  match bytes.from_list([104, 255]) {
    Ok(b) -> println(bytes.to_string(b)) -- Err(invalid UTF-8 at byte 1)
    Err(e) -> println("{e}")
  }
}
```


## `bytes.to_string_lossy`

```
bytes.to_string_lossy(b: Bytes) -> String
```

Decodes `b` as UTF-8 as `bytes.to_string` does, but never fails: each sequence that is no UTF-8 becomes `\u{FFFD}`, shown as `�`.

```silt
import bytes
import string

fn main() {
  match bytes.from_list([104, 105, 255]) {
    Ok(b) -> println(string.length(bytes.to_string_lossy(b))) -- 3
    Err(e) -> println("{e}")
  }
}
```


## Examples

```silt
import bytes
import list

fn main() {
  -- Construction
  let hello = bytes.from_string("hello") -- 5 bytes
  let raw = match bytes.from_hex("deadbeef") { -- 4 bytes
    Ok(b) -> b
    Err(_) -> bytes.empty()
  }

  -- Length and access
  println(bytes.length(hello)) -- 5
  println(bytes.get(hello, 0)) -- Some(104)
  println(bytes.get(hello, 5)) -- None
  println(bytes.length(raw)) -- 4
  println(bytes.is_empty(raw)) -- false
  println(bytes.is_empty(bytes.empty())) -- true

  -- Encoding
  println(bytes.to_hex(hello)) -- 68656c6c6f
  println(bytes.to_base64(hello)) -- aGVsbG8=

  -- Concatenation
  let space = bytes.from_string(" ")
  let world = bytes.from_string("world")
  let greeting = bytes.concat_all([hello, space, world])
  match bytes.to_string(greeting) {
    Ok(s) -> println(s) -- hello world
    Err(e) -> println("{e}")
  }

  -- Slicing (half-open; an index past the end is the end)
  println(bytes.to_hex(bytes.slice(greeting, 6, 11))) -- 776f726c64
  println(bytes.to_hex(bytes.slice(greeting, 6, 99))) -- 776f726c64

  -- Equality is structural
  let a = bytes.from_string("foo")
  let b = bytes.from_string("foo")
  println(a == b) -- true

  -- Order is byte by byte, a shorter prefix first
  println(bytes.from_string("ab") < bytes.from_string("b")) -- true
  println(bytes.from_string("ab") < bytes.from_string("abc")) -- true
  println(bytes.from_string("é") < bytes.from_string("z")) -- false (its first byte, c3, is after 7a)
  let sorted = list.sort([bytes.from_string("b"), bytes.empty()])
  println(list.map(sorted) { v -> bytes.length(v) }) -- [0, 1]

  -- Search / prefix / suffix / split
  let msg = bytes.from_string("foo::bar::baz")
  let sep = bytes.from_string("::")
  match bytes.index_of(msg, sep) {
    Some(i) -> println(i) -- 3
    None -> println(-1)
  }
  println(bytes.contains(msg, sep)) -- true
  println(bytes.contains(msg, bytes.from_string("qux"))) -- false
  println(bytes.starts_with(msg, bytes.from_string("foo"))) -- true
  println(bytes.ends_with(msg, bytes.from_string("baz"))) -- true
  -- bytes.split yields [foo, bar, baz] as three Bytes values.
  let parts = bytes.split(msg, sep)
  println(list.length(parts)) -- 3
}
```

## Errors

Every fallible `bytes.*` call returns `Result(T, BytesError)`. The enum
exposes these variants keyed by the structural failure; pattern-match
for granular handling or show it with `"{e}"`, which gives its message:

| Variant | Fields | Raised by |
|---------|--------|-----------|
| `InvalidUtf8(offset)` | `Int` | `to_string` on non-UTF-8 input |
| `InvalidHex(msg)` | `String` | `from_hex` on odd length / non-hex char |
| `InvalidBase64(msg)` | `String` | `from_base64` on malformed input, or input without its `=` padding (`"aGVsbG8"` for `"aGVsbG8="`) |
| `ByteOutOfRange(value)` | `Int` | `from_list` when an element is negative or `> 255` |

`bytes.get` is a lookup and returns an `Option`; `bytes.slice` cuts its
indices to the bytes there are, as `string.slice` does (a negative one counts
as 0, one past the end as the end), and returns `Bytes` (empty if
`start > end`).

A `BytesError` is shown (`"{e}"`) as its message.

## Notes

- `Bytes` is allocated once and shared by `Arc` internally — passing the
  same `Bytes` through many functions does not copy the underlying buffer.
- Display format is `bytes(<hex preview, up to 32 bytes>, length: N)`,
  intended for debugging output. Use `bytes.to_hex` or `bytes.to_base64`
  for stable serialization.
- `bytes.index_of` returns `Some(0)` for an empty `needle`. `bytes.starts_with`
  and `bytes.ends_with` return `true` for an empty prefix / suffix.
- `bytes.to_string_lossy` decodes as `bytes.to_string` does, but never
  fails: each maximal sequence that is no UTF-8 (a byte that starts no
  character, or a character cut short) becomes one U+FFFD (`�`), as
  Unicode recommends: the two stray bytes `ff fe` are two, the cut
  character `e2 82` is one. It replaces only where a program asks for it: `to_string`,
  `io.read_file` and `stream.file_lines` stay strict. With `io.read_bytes`
  it reads a log file that has stray bytes in it:

  ```silt
  import bytes
  import io
  import list
  import string

  fn main() {
    match io.read_bytes("app.log") {
      Ok(raw) -> {
        let lines = string.split(bytes.to_string_lossy(raw), "\n")
        println(list.length(lines))
      }
      Err(e) -> println("{e}")
    }
  }
  ```
- `bytes.split` splits as `string.split` does: a `sep` at the start or the
  end of `b`, or two side by side, gives an empty piece there, and an empty
  `b` is one empty piece. An empty `sep` splits `b` into its single bytes,
  with no empty piece at either end, as `string.split` with an empty
  separator splits a string into its characters.
