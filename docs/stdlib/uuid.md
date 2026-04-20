---
title: "uuid"
section: "Standard Library"
order: 19
---

# uuid

UUID generation, parsing, and validation. All UUIDs cross the language
boundary as `String` in the canonical lowercase hyphenated form:
`"550e8400-e29b-41d4-a716-446655440000"` (8-4-4-4-12, 36 characters).

Two generators are provided:

- `uuid.v4` — fully random UUIDs (RFC 9562 version 4). Random bits come
  from the OS CSPRNG via the [`getrandom`](https://crates.io/crates/getrandom)
  crate (the same source that backs `crypto.random_bytes`).
- `uuid.v7` — time-ordered UUIDs (RFC 9562 version 7, 2022 spec). The
  first 48 bits encode a Unix millisecond timestamp, the remainder is
  random. Lexicographic string comparison on two v7 UUIDs minted in
  order tracks generation time — ideal for B-tree-friendly primary
  keys where monotonic inserts avoid page splits, while still being
  unguessable.

Use `uuid.parse` when you need to validate and canonicalize a UUID
string at trust boundaries (e.g. parsing an HTTP path param). It
accepts any form the underlying parser understands (hyphenated,
simple/32-char, braced, urn-prefixed) and canonicalizes the output.
Use `uuid.is_valid` when you only need the boolean — no `Result`
allocation.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `v4` | `() -> String` | Random UUID (version 4, CSPRNG-backed) |
| `v7` | `() -> String` | Time-ordered UUID (version 7, RFC 9562) |
| `parse` | `(String) -> Result(String, String)` | Validate + canonicalize any-version UUID |
| `nil` | `() -> String` | The all-zero UUID sentinel |
| `is_valid` | `(String) -> Bool` | Predicate form of `parse` |

## Examples

```silt
import uuid

fn main() {
  -- Random UUID
  let id = uuid.v4()
  println(id)
  -- e.g. "e8400-e29b-41d4-a716-446655440000"

  -- Time-ordered UUID (good for DB primary keys)
  let pk = uuid.v7()
  println(pk)

  -- Two v7s sort by generation time
  let a = uuid.v7()
  let b = uuid.v7()
  -- a < b when compared as strings

  -- Validate + canonicalize external input
  match uuid.parse("550E8400-E29B-41D4-A716-446655440000") {
    Ok(canonical) -> println(canonical)
    -- "550e8400-e29b-41d4-a716-446655440000"
    Err(e) -> println(e)
  }

  -- Predicate form, no Result allocation
  match uuid.is_valid("not-a-uuid") {
    true -> println("valid")
    false -> println("invalid")
  }

  -- Sentinel
  println(uuid.nil())
  -- "00000000-0000-0000-0000-000000000000"
}
```

## Errors

Only `parse` is fallible at the type level; the generators and `nil`
are total.

| Operation | Error condition |
|-----------|-----------------|
| `parse` | Input is not a syntactically valid UUID (`"invalid uuid: ..."`) |

## Notes

- Output is always the 36-character lowercase hyphenated form. If you
  need a different shape (braced, simple/32-char, uppercase), transform
  the returned string with `string` helpers — keeping one canonical
  wire format here avoids a combinatorial API surface.
- `uuid.parse` is version-agnostic: it accepts v1 through v8 UUIDs and
  the nil UUID. If you need to *reject* particular versions, match on
  the relevant substring of the returned canonical form; a dedicated
  `uuid.version` accessor can land later if demand warrants it.
- `uuid.v4` and `uuid.v7` collide with probability ~2^-122 and
  ~2^-74-per-ms respectively — treat collisions as impossible in
  practice, not as a case to handle at call sites.
- Prefer `uuid.v7` over `uuid.v4` for database primary keys: monotonic
  inserts keep B-tree pages hot and avoid the random-write amplification
  that v4 causes on sorted indexes.
