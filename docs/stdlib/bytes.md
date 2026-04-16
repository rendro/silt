---
title: "bytes"
section: "Standard Library"
order: 16
---

# bytes

Immutable byte sequences. The `Bytes` value type carries arbitrary binary
data — useful for protocol parsing, file I/O, hashing, encoding/decoding,
and (when paired with `tcp` from v0.9) network communication.

`Bytes` values use **structural equality**: two byte sequences with the
same content compare equal regardless of how they were constructed. They
work as `Map`/`Set` keys and respect the standard `==` operator.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `concat` | `(Bytes, Bytes) -> Bytes` | Concatenate two byte sequences |
| `concat_all` | `(List(Bytes)) -> Bytes` | Concatenate every element of a list |
| `empty` | `() -> Bytes` | Zero-length byte sequence |
| `eq` | `(Bytes, Bytes) -> Bool` | Structural byte-by-byte comparison |
| `from_base64` | `(String) -> Result(Bytes, String)` | Decode base64 string |
| `from_hex` | `(String) -> Result(Bytes, String)` | Decode hex string (case-insensitive) |
| `from_list` | `(List(Int)) -> Result(Bytes, String)` | Build from a list of byte values (0..=255) |
| `from_string` | `(String) -> Bytes` | UTF-8 encode a string |
| `get` | `(Bytes, Int) -> Result(Int, String)` | Read a single byte at index |
| `length` | `(Bytes) -> Int` | Number of bytes |
| `slice` | `(Bytes, Int, Int) -> Result(Bytes, String)` | Half-open `[start, end)` slice |
| `to_base64` | `(Bytes) -> String` | Encode as base64 |
| `to_hex` | `(Bytes) -> String` | Encode as lowercase hex |
| `to_list` | `(Bytes) -> List(Int)` | Materialize as a list of byte values |
| `to_string` | `(Bytes) -> Result(String, String)` | UTF-8 decode (errors on invalid UTF-8) |

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
    Err(e) -> println(e)
  }

  -- Encoding
  println(bytes.to_hex(hello))                     -- "68656c6c6f"
  println(bytes.to_base64(hello))                  -- "aGVsbG8="

  -- Concatenation
  let space = bytes.from_string(" ")
  let world = bytes.from_string("world")
  let greeting = bytes.concat_all([hello, space, world])
  match bytes.to_string(greeting) {
    Ok(s) -> println(s)                            -- "hello world"
    Err(e) -> println(e)
  }

  -- Slicing (half-open)
  match bytes.slice(greeting, 6, 11) {
    Ok(s) -> println(bytes.to_hex(s))              -- "776f726c64"
    Err(e) -> println(e)
  }

  -- Equality is structural
  let a = bytes.from_string("foo")
  let b = bytes.from_string("foo")
  println(a == b)                                  -- true
}
```

## Errors

The fallible operations all return `Result(_, String)` with a descriptive
error message:

| Operation | Error condition |
|-----------|-----------------|
| `from_hex` | Odd-length string, or non-hex character |
| `from_base64` | Malformed base64 |
| `from_list` | Element outside `0..=255`, or negative |
| `to_string` | Invalid UTF-8 |
| `slice` | Negative bounds, `start > end`, or `end > length` |
| `get` | Negative index, or `i >= length` |

## Notes

- `Bytes` is allocated once and shared by `Arc` internally — passing the
  same `Bytes` through many functions does not copy the underlying buffer.
- Display format is `bytes(<hex preview, up to 32 bytes>, length: N)`,
  intended for debugging output. Use `bytes.to_hex` or `bytes.to_base64`
  for stable serialization.
- A future silt release may promote `Bytes` to a language-level type with
  literal syntax (e.g. `b"hello"`) and method-form access. Today's API
  is forward-compatible: programs written against the v0.9 module surface
  will continue to behave identically.
