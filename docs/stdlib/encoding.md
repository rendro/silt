---
title: "encoding"
section: "Standard Library"
order: 18
---

# encoding

URL / percent encoding per [RFC 3986](https://www.rfc-editor.org/rfc/rfc3986).
This module is intentionally narrow: base64 and hex encoding live on
the [`bytes`](bytes.md) module (`bytes.to_base64` / `bytes.from_base64` /
`bytes.to_hex` / `bytes.from_hex`) because they operate on `Bytes`, not
`String`. Percent-encoding, by contrast, is a `String` ↔ `String`
transform — the input is text destined for a URL (query-string value,
path segment, fragment) and the output is text.

RFC 3986 §2.3 defines the **unreserved** set that never needs encoding:

```
ALPHA / DIGIT / "-" / "." / "_" / "~"
```

Every other byte of the UTF-8 representation is emitted as `%HH` with
upper-case hex digits (per §6.2.2.1's case normalization note). Decoding
is case-insensitive: `%2F` and `%2f` both decode to `/`.

`+` is a **literal `+`** in both directions. The `+ ↔ space` convention
belongs to `application/x-www-form-urlencoded` (WHATWG URL §form-urlencoded),
not RFC 3986, and is out of scope here. Build form-encoding on top of
`encoding.url_encode` in a dedicated module if you need it.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `url_encode` | `(String) -> String` | Percent-encode per RFC 3986 (unreserved = `ALPHA` / `DIGIT` / `-._~`) |
| `url_decode` | `(String) -> Result(String, String)` | Inverse. Errors on malformed `%HH` or invalid UTF-8 after decoding |

## Examples

```silt
import encoding

fn main() {
  -- Safely embed user-supplied text in a query-string value.
  let query = "hello world & goodbye?"
  let encoded = encoding.url_encode(query)
  println(encoded)
  -- hello%20world%20%26%20goodbye%3F

  -- Round-trip.
  match encoding.url_decode(encoded) {
    Ok(back) -> println(back)              -- hello world & goodbye?
    Err(e) -> println(e)
  }

  -- Non-ASCII: UTF-8 bytes are encoded.
  println(encoding.url_encode("café"))     -- caf%C3%A9

  -- Malformed input is rejected.
  match encoding.url_decode("bad%") {
    Ok(_) -> println("should not happen")
    Err(e) -> println(e)                   -- truncated percent-escape at offset 3
  }
}
```

## Errors

Only `url_decode` is fallible — `url_encode` is a total function over
any `String`.

| Operation | Error condition |
|-----------|-----------------|
| `url_decode` | Truncated `%` at end of string (e.g. `"bad%"`) |
| `url_decode` | Non-hex digits after `%` (e.g. `"bad%ZZ"`) |
| `url_decode` | Decoded byte sequence is not valid UTF-8 (e.g. `"%C3%28"`) |

## Notes

- The encoder always emits upper-case hex (`%2F`, not `%2f`). The
  decoder accepts both cases. This matches the RFC's case-normalization
  recommendation (§6.2.2.1) for producers.
- `url_encode` is *not* a query-string builder. For `key=value&key=value`
  assembly, `url_encode` each key and each value separately and join
  the resulting strings yourself. That separation keeps the primitive
  honest — you can use it for path segments, fragments, and header
  values too, not just query parameters.
- Binary payloads should go through `bytes.to_base64` (or `bytes.to_hex`)
  first, then the resulting ASCII string can be fed to `url_encode` if
  it still needs URL-safety on top of base64.
