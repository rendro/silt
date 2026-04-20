---
title: "crypto"
section: "Standard Library"
order: 17
---

# crypto

Cryptographic primitives: SHA-256 / SHA-512 hashing, HMAC message
authentication, an OS-backed CSPRNG, and a timing-safe byte-comparison
routine. All functions consume and produce `Bytes` values — pipe through
`bytes.to_hex` / `bytes.to_base64` when you need a string representation.

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
| `hmac_sha256` | `(Bytes, Bytes) -> Bytes` | HMAC-SHA256 over `(key, msg)` (32 bytes) |
| `hmac_sha512` | `(Bytes, Bytes) -> Bytes` | HMAC-SHA512 over `(key, msg)` (64 bytes) |
| `random_bytes` | `(Int) -> Result(Bytes, String)` | OS CSPRNG, `0..=1_048_576` bytes |
| `constant_time_eq` | `(Bytes, Bytes) -> Bool` | Timing-safe comparison (lengths leak) |

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
  println(bytes.length(tag))                       -- 32

  -- CSPRNG: 32 random bytes (256-bit token)
  match crypto.random_bytes(32) {
    Ok(token) -> println(bytes.to_hex(token))
    Err(e) -> println(e)
  }

  -- Timing-safe comparison for auth tag verification
  let expected = crypto.hmac_sha256(key, bytes.from_string("hello"))
  let received = crypto.hmac_sha256(key, bytes.from_string("hello"))
  println(crypto.constant_time_eq(expected, received))  -- true
}
```

## Errors

Only `random_bytes` is fallible at the type level — the others are
total functions over their `Bytes` inputs.

| Operation | Error condition |
|-----------|-----------------|
| `random_bytes` | `n < 0` ("n must be non-negative"); `n > 1_048_576` ("n exceeds 1 MiB cap"); OS CSPRNG failure |

## Notes

- Digest outputs are always exactly `32` bytes (`sha256`, `hmac_sha256`)
  or `64` bytes (`sha512`, `hmac_sha512`). The typechecker has no
  dependent-type support for fixed-width `Bytes`, so the returned type
  is the same opaque `Bytes` the rest of the module uses.
- `random_bytes(0)` returns an empty `Bytes` (not an error). This keeps
  caller code simpler when the length comes from a variable.
- The 1 MiB cap on `random_bytes` is a sanity guard against accidental
  huge allocations; it is not a security boundary. Loop if you really
  need more than a mebibyte of entropy at a time.
- Use `crypto.constant_time_eq` — not `bytes.eq` and not the `==`
  operator — whenever you compare a user-supplied value against a
  secret (MAC tag, password hash, token). The `bytes.eq` path
  short-circuits on the first differing byte and leaks the matching
  prefix length via timing.
