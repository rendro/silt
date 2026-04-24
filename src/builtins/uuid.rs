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

use super::common::{err, ok, require_string};
use crate::value::Value;
use crate::vm::{Vm, VmError};

/// Dispatch `uuid.<name>(args)`.
pub fn call(_vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "v4" => v4(args),
        "v7" => v7(args),
        "parse" => parse(args),
        "nil" => nil(args),
        "is_valid" => is_valid(args),
        _ => Err(VmError::new(format!("unknown uuid function: {name}"))),
    }
}

// ── Generators ─────────────────────────────────────────────────────────

/// `uuid.v4() -> String` — generate a random (version 4) UUID. Random
/// bits come from the OS CSPRNG via the `getrandom` crate. Returned as
/// the canonical lowercase hyphenated form.
fn v4(args: &[Value]) -> Result<Value, VmError> {
    if !args.is_empty() {
        return Err(VmError::new("uuid.v4 takes no arguments".into()));
    }
    Ok(Value::String(::uuid::Uuid::new_v4().to_string()))
}

/// `uuid.v7() -> String` — generate a time-ordered (version 7) UUID
/// per RFC 9562. The first 48 bits encode a Unix millisecond timestamp,
/// the remaining bits are random, so two v7 UUIDs minted in order
/// compare correctly via lexicographic string comparison. Good for
/// B-tree primary keys.
fn v7(args: &[Value]) -> Result<Value, VmError> {
    if !args.is_empty() {
        return Err(VmError::new("uuid.v7 takes no arguments".into()));
    }
    Ok(Value::String(::uuid::Uuid::now_v7().to_string()))
}

// ── Parse / validate / nil ─────────────────────────────────────────────

/// `uuid.parse(s: String) -> Result(String, String)` — validate and
/// canonicalize a UUID string. Accepts any form the underlying parser
/// understands (hyphenated, simple/32-char, braced, urn-prefixed) and
/// returns the lowercase hyphenated canonical form on success. Returns
/// `Err(msg)` on malformed input.
fn parse(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("uuid.parse takes 1 argument".into()));
    }
    let s = require_string(&args[0], "uuid.parse")?;
    match ::uuid::Uuid::parse_str(&s) {
        Ok(u) => Ok(ok(Value::String(u.hyphenated().to_string()))),
        Err(e) => Ok(err(format!("invalid uuid: {e}"))),
    }
}

/// `uuid.nil() -> String` — the all-zero UUID,
/// `"00000000-0000-0000-0000-000000000000"`. Useful as a sentinel
/// value where a `None`-style Option(String) would be overkill.
fn nil(args: &[Value]) -> Result<Value, VmError> {
    if !args.is_empty() {
        return Err(VmError::new("uuid.nil takes no arguments".into()));
    }
    Ok(Value::String(::uuid::Uuid::nil().hyphenated().to_string()))
}

/// `uuid.is_valid(s: String) -> Bool` — predicate form of `parse`. Does
/// not allocate the `Result` wrapper, suitable for hot-path checks
/// where the caller only cares whether the input parses.
fn is_valid(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("uuid.is_valid takes 1 argument".into()));
    }
    let s = require_string(&args[0], "uuid.is_valid")?;
    Ok(Value::Bool(::uuid::Uuid::parse_str(&s).is_ok()))
}
