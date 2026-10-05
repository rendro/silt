---
title: "bytes"
section: "Standard Library"
order: 16
---

# bytes

Immutable byte sequences. The `Bytes` value type carries arbitrary binary
data — useful for protocol parsing, file I/O, hashing, encoding/decoding,
and (when paired with `tcp`) network communication.

`Bytes` values use **structural equality**: two byte sequences with the
same content compare equal regardless of how they were constructed. They
work as `Map`/`Set` keys and respect the standard `==` operator.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `concat` | `(Bytes, Bytes) -> Bytes` | Concatenate two byte sequences |
| `concat_all` | `(List(Bytes)) -> Bytes` | Concatenate every element of a list |
| `empty` | `() -> Bytes` | Zero-length byte sequence |
| `ends_with` | `(Bytes, Bytes) -> Bool` | True if `b` ends with `suffix` |
| `eq` | `(Bytes, Bytes) -> Bool` | Structural byte-by-byte comparison |
| `from_base64` | `(String) -> Result(Bytes, BytesError)` | Decode base64 string |
| `from_hex` | `(String) -> Result(Bytes, BytesError)` | Decode hex string (case-insensitive) |
| `from_list` | `(List(Int)) -> Result(Bytes, BytesError)` | Build from a list of byte values (0..=255) |
| `from_string` | `(String) -> Bytes` | UTF-8 encode a string |
| `get` | `(Bytes, Int) -> Result(Int, BytesError)` | Read a single byte at index |
| `index_of` | `(Bytes, Bytes) -> Option(Int)` | First offset at which `needle` appears |
| `length` | `(Bytes) -> Int` | Number of bytes |
| `slice` | `(Bytes, Int, Int) -> Result(Bytes, BytesError)` | Half-open `[start, end)` slice |
| `split` | `(Bytes, Bytes) -> List(Bytes)` | Split on every occurrence of `sep` (panics if `sep` is empty) |
| `starts_with` | `(Bytes, Bytes) -> Bool` | True if `b` begins with `prefix` |
| `to_base64` | `(Bytes) -> String` | Encode as base64 |
| `to_hex` | `(Bytes) -> String` | Encode as lowercase hex |
| `to_list` | `(Bytes) -> List(Int)` | Materialize as a list of byte values |
| `to_string` | `(Bytes) -> Result(String, BytesError)` | UTF-8 decode (errors on invalid UTF-8) |

## Examples

```silt
import bytes

fn main() {
  -- Construction
  let hello = bytes.from_string("hello")           -- 5 bytes
  let raw = match bytes.from_hex("deadbeef") {     -- 4 bytes
    Ok(b) -> b
    Err(_) -> bytes.empty()
  }

  -- Length and access
  println(bytes.length(hello))                     -- 5
  match bytes.get(hello, 0) {
    Ok(n) -> println(n)                            -- 104
    Err(e) -> println(e.message())
  }

  -- Encoding
  println(bytes.to_hex(hello))                     -- 68656c6c6f
  println(bytes.to_base64(hello))                  -- aGVsbG8=

  -- Concatenation
  let space = bytes.from_string(" ")
  let world = bytes.from_string("world")
  let greeting = bytes.concat_all([hello, space, world])
  match bytes.to_string(greeting) {
    Ok(s) -> println(s)                            -- hello world
    Err(e) -> println(e.message())
  }

  -- Slicing (half-open)
  match bytes.slice(greeting, 6, 11) {
    Ok(s) -> println(bytes.to_hex(s))              -- 776f726c64
    Err(e) -> println(e.message())
  }

  -- Equality is structural
  let a = bytes.from_string("foo")
  let b = bytes.from_string("foo")
  println(a == b)                                  -- true

  -- Search / prefix / suffix / split
  let msg = bytes.from_string("foo::bar::baz")
  let sep = bytes.from_string("::")
  match bytes.index_of(msg, sep) {
    Some(i) -> println(i)                          -- 3
    None -> println(-1)
  }
  println(bytes.starts_with(msg, bytes.from_string("foo")))  -- true
  println(bytes.ends_with(msg, bytes.from_string("baz")))    -- true
  -- bytes.split yields [foo, bar, baz] as three Bytes values.
  let parts = bytes.split(msg, sep)
}
```

## Errors

Every fallible `bytes.*` call returns `Result(T, BytesError)`. The enum
exposes five variants keyed by the structural failure; pattern-match
for granular handling or fall back to `e.message()`:

| Variant | Fields | Raised by |
|---------|--------|-----------|
| `BytesInvalidUtf8(offset)` | `Int` | `to_string` on non-UTF-8 input |
| `BytesInvalidHex(msg)` | `String` | `from_hex` on odd length / non-hex char |
| `BytesInvalidBase64(msg)` | `String` | `from_base64` on malformed input |
| `BytesByteOutOfRange(value)` | `Int` | `from_list` when an element is negative or `> 255` |
| `BytesOutOfBounds(index)` | `Int` | `slice` / `get` on an out-of-range index |

`BytesError` implements the built-in `Error` trait.

## Notes

- `Bytes` is allocated once and shared by `Arc` internally — passing the
  same `Bytes` through many functions does not copy the underlying buffer.
- Display format is `bytes(<hex preview, up to 32 bytes>, length: N)`,
  intended for debugging output. Use `bytes.to_hex` or `bytes.to_base64`
  for stable serialization.
- `bytes.index_of` returns `Some(0)` for an empty `needle`. `bytes.starts_with`
  and `bytes.ends_with` return `true` for an empty prefix / suffix.
- `bytes.split` panics if `sep` is empty (ambiguous). Splitting an empty
  `b` yields `[empty_bytes]` — one element — mirroring `string.split`.
- A future silt release may promote `Bytes` to a language-level type with
  literal syntax (e.g. `b"hello"`) and method-form access. Today's API
  is forward-compatible: programs written against the current `bytes`
  module surface will continue to behave identically.
