---
title: "crypto"
section: "Standard Library"
order: 19
---

# crypto

Cryptographic primitives: SHA-256 / SHA-512 / BLAKE2b / MD5 hashing,
HMAC message authentication, an OS-backed CSPRNG, and a timing-safe
byte-comparison routine. Most functions consume and produce `Bytes`
values — pipe through `bytes.to_hex` / `bytes.to_base64` when you need
a string representation.

`crypto.random_bytes` reads directly from the operating system's
cryptographically secure pseudo-random number generator (`getrandom(2)`
on Linux, `BCryptGenRandom` on Windows, `SecRandomCopyBytes` on macOS;
see the [getrandom](https://crates.io/crates/getrandom) crate for the
full platform list). The `crypto.constant_time_eq` comparison is
timing-safe for equal-length inputs: the running time is independent of
where a mismatch occurs. Length mismatches short-circuit to `false`,
matching the convention used by Python's `hmac.compare_digest` and
Rust's `subtle` crate — this means lengths can leak via timing but the
contents of equal-length buffers cannot. Pad inputs to a fixed width
beforehand if length privacy matters for your protocol.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `sha256` | `(Bytes) -> Bytes` | SHA-256 digest (32 bytes) |
| `sha512` | `(Bytes) -> Bytes` | SHA-512 digest (64 bytes) |
| `md5` | `(Bytes) -> Bytes` | MD5 digest (16 bytes) — **legacy / non-security use only** |
| `blake2b` | `(Bytes) -> Bytes` | BLAKE2b-512 digest (64 bytes), RFC 7693 |
| `hmac_sha256` | `(Bytes, Bytes) -> Bytes` | HMAC-SHA256 over `(key, msg)` (32 bytes) |
| `hmac_sha512` | `(Bytes, Bytes) -> Bytes` | HMAC-SHA512 over `(key, msg)` (64 bytes) |
| `random_bytes` | `(Int) -> Result(Bytes, CryptoError)` | OS CSPRNG, `0..=1_048_576` bytes |
| `constant_time_eq` | `(Bytes, Bytes) -> Bool` | Timing-safe comparison (lengths leak) |

## `crypto.sha256`

```
crypto.sha256(data: Bytes) -> Bytes
```

The SHA-256 digest of `data`: 32 bytes.

```silt
import bytes
import crypto

fn main() {
  let d = crypto.sha256(bytes.from_string("abc"))
  println(bytes.length(d)) -- 32
  println(bytes.to_hex(d)) -- ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
}
```


## `crypto.sha512`

```
crypto.sha512(data: Bytes) -> Bytes
```

The SHA-512 digest of `data`: 64 bytes.

```silt
import bytes
import crypto
import string

fn main() {
  let d = crypto.sha512(bytes.from_string("abc"))
  println(bytes.length(d)) -- 64
  println(string.slice(bytes.to_hex(d), 0, 16)) -- ddaf35a193617aba
}
```


## `crypto.md5`

```
crypto.md5(data: Bytes) -> Bytes
```

The MD5 digest of `data`: 16 bytes. MD5 is not collision-resistant: use it only to match a system that asks for it, never for a security decision.

```silt
import bytes
import crypto

fn main() {
  println(bytes.to_hex(crypto.md5(bytes.from_string("abc")))) -- 900150983cd24fb0d6963f7d28e17f72
}
```


## `crypto.blake2b`

```
crypto.blake2b(data: Bytes) -> Bytes
```

The BLAKE2b-512 digest of `data` (RFC 7693): 64 bytes.

```silt
import bytes
import crypto
import string

fn main() {
  let d = crypto.blake2b(bytes.from_string("abc"))
  println(bytes.length(d)) -- 64
  println(string.slice(bytes.to_hex(d), 0, 16)) -- ba80a53f981c4d0d
}
```


## `crypto.hmac_sha256`

```
crypto.hmac_sha256(key: Bytes, msg: Bytes) -> Bytes
```

The HMAC-SHA256 of `msg` under `key`: 32 bytes. Compare a tag with `crypto.constant_time_eq`.

```silt
import bytes
import crypto

fn main() {
  let tag = crypto.hmac_sha256(bytes.from_string("key"), bytes.from_string("message"))
  println(bytes.to_hex(tag)) -- 6e9ef29b75fffc5b7abae527d58fdadb2fe42e7219011976917343065f58ed4a
}
```


## `crypto.hmac_sha512`

```
crypto.hmac_sha512(key: Bytes, msg: Bytes) -> Bytes
```

The HMAC-SHA512 of `msg` under `key`: 64 bytes.

```silt
import bytes
import crypto

fn main() {
  let tag = crypto.hmac_sha512(bytes.from_string("key"), bytes.from_string("message"))
  println(bytes.length(tag)) -- 64
}
```


## `crypto.random_bytes`

```
crypto.random_bytes(n: Int) -> Result(Bytes, CryptoError)
```

`n` bytes from the operating system's secure random number generator. `n` from 0 to 1,048,576; another is `Err(InvalidLength(n))`, and a failure of the generator `Err(Other(msg))`.

```silt
import bytes
import crypto

fn main() {
  match crypto.random_bytes(16) {
    Ok(token) -> println(bytes.to_hex(token))
    Err(e) -> println("{e}")
  }
}
```


## `crypto.constant_time_eq`

```
crypto.constant_time_eq(a: Bytes, b: Bytes) -> Bool
```

Whether `a` and `b` hold the same bytes, in a time that does not depend on where they differ. Values of two lengths are `false` at once, so the length can show in the time; the contents cannot.

```silt
import bytes
import crypto

fn main() {
  let key = bytes.from_string("key")
  let expected = crypto.hmac_sha256(key, bytes.from_string("order 42"))
  let received = crypto.hmac_sha256(key, bytes.from_string("order 42"))
  println(crypto.constant_time_eq(expected, received)) -- true
  println(crypto.constant_time_eq(expected, bytes.from_string("x"))) -- false
}
```


## Examples

```silt
import bytes
import crypto

fn main() {
  -- Hashing
  let msg = bytes.from_string("abc")
  let digest = crypto.sha256(msg)
  println(bytes.to_hex(digest))
  -- ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad

  -- HMAC authentication
  let key = bytes.from_string("secret key")
  let tag = crypto.hmac_sha256(key, bytes.from_string("hello"))
  println(bytes.length(tag)) -- 32

  -- CSPRNG: 32 random bytes (256-bit token)
  match crypto.random_bytes(32) {
    Ok(token) -> println(bytes.to_hex(token))
    Err(e) -> println(e)
  }

  -- Timing-safe comparison for auth tag verification
  let expected = crypto.hmac_sha256(key, bytes.from_string("hello"))
  let received = crypto.hmac_sha256(key, bytes.from_string("hello"))
  println(crypto.constant_time_eq(expected, received)) -- true
}
```

## Errors

Only `random_bytes` is fallible at the type level — the others are
total functions over their `Bytes` inputs. It returns
`Result(Bytes, CryptoError)`:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `InvalidLength(Int)` | the length asked for | `n < 0` or `n > 1_048_576` |
| `Other(String)` | message | the OS CSPRNG failed |

## Notes

- Digest outputs have fixed widths: `16` bytes (`md5`), `32` bytes
  (`sha256`, `hmac_sha256`), `64` bytes (`sha512`, `hmac_sha512`,
  `blake2b`). The typechecker has no dependent-type support for
  fixed-width `Bytes`, so the returned type is the same opaque `Bytes`
  the rest of the module uses.
- **MD5 is not collision-resistant.** Use it only for interop with
  legacy systems (etags, Git-style content hashing, cache keys where
  an adversary isn't in play). Never use MD5 for signatures,
  certificates, or any security decision — prefer `sha256` or
  `blake2b`.
- `blake2b` is BLAKE2b-512 (the 512-bit-output variant from RFC 7693).
  It's faster than SHA-512 on 64-bit hardware and is a sound default
  for new protocols that don't need to match a SHA-family spec.
- `random_bytes(0)` returns an empty `Bytes` (not an error). This keeps
  caller code simpler when the length comes from a variable.
- The 1 MiB cap on `random_bytes` is a sanity guard against accidental
  huge allocations; it is not a security boundary. Loop if you really
  need more than a mebibyte of entropy at a time.
- Use `crypto.constant_time_eq` — not the `==` operator — whenever you
  compare a user-supplied value against a secret (MAC tag, password
  hash, token). `==` short-circuits on the first differing byte and
  leaks the matching prefix length via timing.
