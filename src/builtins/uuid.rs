//! `uuid.*` builtin functions: UUID generation, parsing, and
//! validation. All functions return UUIDs as lowercase hyphenated
//! strings in the canonical 8-4-4-4-12 form
//! (e.g. `"550e8400-e29b-41d4-a716-446655440000"`).
//!
//! Generators are backed by the `uuid` crate. `uuid.v4` pulls random
//! bits from the OS CSPRNG via `getrandom`; `uuid.v7` combines a 48-bit
//! Unix-millisecond timestamp with random tail bits per RFC 9562, so
//! lexicographic string ordering tracks generation time — useful for
//! B-tree-friendly primary keys.
//!
//! `uuid.parse` and `uuid.is_valid` are version-agnostic: any syntactic
//! UUID (v1..v8, or the nil UUID) is accepted. The `parse` form
//! canonicalizes to lowercase hyphenated output regardless of the input
//! casing or formatting accepted by the underlying parser (hyphenated,
//! braced, urn-prefixed, simple/32-char).

use super::common::{err, ok};
use super::typed::builtins;
use crate::value::Value;
use crate::vm::Vm;

/// A version 7 UUID whose timestamp is the host clock's time. The VM's
/// counter keeps the UUIDs minted within a millisecond in order.
pub(crate) fn now_v7(vm: &Vm) -> ::uuid::Uuid {
    let now = vm.runtime.io.now();
    ::uuid::Uuid::new_v7(::uuid::Timestamp::from_unix(
        &vm.runtime.uuid_v7,
        now.as_secs(),
        now.subsec_nanos(),
    ))
}

builtins! {
    // ── Generators ─────────────────────────────────────────────────────

    /// A random (version 4) UUID. Random bits come from the OS CSPRNG
    /// via the `getrandom` crate. Returned as the canonical lowercase
    /// hyphenated form.
    fn v4() -> String {
        ::uuid::Uuid::new_v4().to_string()
    }

    /// A time-ordered (version 7) UUID per RFC 9562. The first 48 bits
    /// encode a Unix millisecond timestamp, the remaining bits are
    /// random, so two v7 UUIDs minted in order compare correctly via
    /// lexicographic string comparison. Good for B-tree primary keys.
    fn v7(vm) -> String {
        now_v7(vm).to_string()
    }

    // ── Parse / validate / nil ─────────────────────────────────────────

    /// Validates and canonicalizes a UUID string. Accepts any form the
    /// underlying parser understands (hyphenated, simple/32-char,
    /// braced, urn-prefixed) and returns the lowercase hyphenated
    /// canonical form on success, `Err(msg)` on malformed input.
    fn parse(s: &str) -> Value {
        match ::uuid::Uuid::parse_str(s) {
            Ok(u) => ok(Value::String(u.hyphenated().to_string().into())),
            Err(e) => err(format!("invalid uuid: {e}")),
        }
    }

    /// The all-zero UUID, `"00000000-0000-0000-0000-000000000000"`.
    /// Useful as a sentinel value where a `None`-style Option(String)
    /// would be overkill.
    fn nil() -> String {
        ::uuid::Uuid::nil().hyphenated().to_string()
    }

    /// Predicate form of `parse`.
    fn is_valid(s: &str) -> bool {
        ::uuid::Uuid::parse_str(s).is_ok()
    }
}
