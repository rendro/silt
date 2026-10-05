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
| `form_encode` | `(List((String, String))) -> String` | Build an `application/x-www-form-urlencoded` body |
| `form_decode` | `(String) -> Result(List((String, String)), String)` | Parse an `application/x-www-form-urlencoded` body into pairs |

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

## `form_encode`

```
encoding.form_encode(pairs: List((String, String))) -> String
```

Produces an `application/x-www-form-urlencoded` body. Each `(key, value)`
pair becomes `key=value`; both halves are percent-escaped with the
WHATWG form-urlencoded byte set (space → `+`, `*-._` plus
alphanumerics pass through, everything else becomes `%HH` with
upper-case hex); pairs are joined with `&`. Input order is preserved
in the output, so callers can build deterministic signatures. An empty
list produces the empty string.

```silt
import encoding

fn main() {
  let body = encoding.form_encode([
    ("name", "Ada Lovelace"),
    ("role", "analyst & author"),
    ("lang", "English"),
  ])
  println(body)
  -- name=Ada+Lovelace&role=analyst+%26+author&lang=English
}
```

The signature takes `List((String, String))` rather than
`Map(String, String)` on purpose: order matters for APIs that sign or
hash the encoded body (OAuth 1.0a, S3 canonical query strings, etc.),
and a `List` preserves it. It also lets callers represent duplicate
keys, which are legal in form bodies.

## `form_decode`

```
encoding.form_decode(body: String) -> Result(List((String, String)), String)
```

Inverse of `form_encode`. Splits the body on `&`, splits each segment
on its first `=`, and decodes both halves: `+` becomes a space, `%HH`
becomes the corresponding byte, and the combined byte sequence must be
valid UTF-8. A segment with no `=` is treated as `(key, "")`. Empty
segments (produced by leading, trailing, or doubled `&`) are silently
skipped, matching the WHATWG URL parser. Order is preserved.

```silt
import encoding

fn main() {
  match encoding.form_decode("a=1&b=hello+world&c=%26") {
    Ok(pairs) -> println(pairs)
    -- [("a", "1"), ("b", "hello world"), ("c", "&")]
    Err(e) -> println(e)
  }
}
```

Malformed percent escapes or invalid UTF-8 surface as `Err(msg)` with
a message identifying which pair and half (key / value) was bad.
