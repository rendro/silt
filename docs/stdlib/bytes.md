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
    Err(e) -> println(Error.message(e))
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
for granular handling or fall back to `Error.message(e)`:

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

`BytesError` implements the built-in `Error` trait.

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
      Err(e) -> println(Error.message(e))
    }
  }
  ```
- `bytes.split` splits as `string.split` does: a `sep` at the start or the
  end of `b`, or two side by side, gives an empty piece there, and an empty
  `b` is one empty piece. An empty `sep` splits `b` into its single bytes,
  with no empty piece at either end, as `string.split` with an empty
  separator splits a string into its characters.
