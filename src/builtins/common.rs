//! Shared helper functions used by several builtin modules.
//!
//! Round 57 collapsed four copy-pasted sets of helpers (`ok`, `err`,
//! `require_string`, `require_int`, `require_bytes`, `value_kind`) that
//! lived verbatim in `uuid.rs`, `encoding.rs`, `crypto.rs`, and
//! `bytes.rs`. Each call site had the same bodies and the same error
//! phrasing, so we hoist one canonical copy here.
//!
//! `require_str_borrow` is preserved alongside `require_string` because
//! its signature genuinely differs (returns `&str` rather than cloning
//! into `String`). The `err()` helpers stay local in each builtin
//! module because they wrap module-specific error variants like
//! `TcpUnknown` / `StreamErr`.

use std::sync::Arc;

use crate::value::Value;
use crate::vm::VmError;

pub(crate) fn ok(v: Value) -> Value {
    Value::Variant("Ok".into(), vec![v])
}

pub(super) fn err(s: impl Into<String>) -> Value {
    Value::Variant("Err".into(), vec![Value::String(s.into())])
}

pub(super) fn require_string(arg: &Value, fn_label: &str) -> Result<String, VmError> {
    match arg {
        Value::String(s) => Ok(s.clone()),
        other => Err(VmError::new(format!(
            "{fn_label} requires String, got {}",
            value_kind(other)
        ))),
    }
}

pub(super) fn require_int(arg: &Value, fn_label: &str) -> Result<i64, VmError> {
    match arg {
        Value::Int(n) => Ok(*n),
        other => Err(VmError::new(format!(
            "{fn_label} requires Int, got {}",
            value_kind(other)
        ))),
    }
}

pub(super) fn require_bytes(arg: &Value, fn_label: &str) -> Result<Arc<Vec<u8>>, VmError> {
    match arg {
        Value::Bytes(b) => Ok(b.clone()),
        other => Err(VmError::new(format!(
            "{fn_label} requires Bytes, got {}",
            value_kind(other)
        ))),
    }
}

/// Surface a `Value`'s kind as a TitleCase `&'static str`. Used by the
/// canonical `"<fn> requires <Kind>, got <kind>"` diagnostic shape that
/// round 73f / 74 / 75 standardised across `numeric.rs`, `string.rs`,
/// `collections.rs`, `bytes.rs`, `crypto.rs`, `encoding.rs`, and `uuid.rs`.
///
/// **Round 75 — "name the offending kind" exhaustiveness.** Pre-fix the
/// arm list covered eight variants and collapsed every other shape with a
/// `_ => "value"` fallthrough. That fallthrough hid Map/Set/Range/Variant/
/// Record/Unit/Channel/Handle/etc. behind a generic word in the diagnostic
/// — exactly the silent-wrong-answer mode round 73f opposed. Each variant
/// is now enumerated explicitly, with naming mirrored from
/// `vm::Vm::type_name` (the canonical type-name oracle).
///
/// `pub(crate)` so a test-only re-export at `silt::builtins::value_kind`
/// can pin the matrix from an integration test
/// (`tests/round75_kind_naming_canonical_tests.rs`). `mod common` itself
/// remains private — only this single helper crosses the module wall.
pub(crate) fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::ExtFloat(_) => "ExtFloat",
        Value::Bool(_) => "Bool",
        Value::String(_) => "String",
        Value::List(_) => "List",
        Value::Range(..) => "Range",
        Value::Map(_) => "Map",
        Value::Set(_) => "Set",
        Value::Tuple(_) => "Tuple",
        Value::Record(..) => "Record",
        Value::Variant(..) => "Variant",
        // VmClosure surfaces as "Fn" (matches `Type::Fun` Display and the
        // canonical dispatch name) — round 71 follow-up unification.
        Value::VmClosure(_) => "Fn",
        Value::BuiltinFn(_) => "BuiltinFn",
        Value::VariantConstructor(..) => "VariantConstructor",
        Value::TypeDescriptor(_) => "TypeDescriptor",
        Value::PrimitiveDescriptor(_) => "PrimitiveDescriptor",
        Value::Channel(_) => "Channel",
        Value::Handle(_) => "Handle",
        Value::Bytes(_) => "Bytes",
        Value::TcpListener(_) => "TcpListener",
        Value::TcpStream(_) => "TcpStream",
        Value::Unit => "Unit",
    }
}

/// `require_string` variant that borrows the underlying `&str` instead
/// of cloning into a `String`. Shares `tcp.rs` / `stream.rs` byte-
/// identical implementations (round 65 dedup).
pub(super) fn require_str_borrow<'a>(arg: &'a Value, fn_label: &str) -> Result<&'a str, VmError> {
    match arg {
        Value::String(s) => Ok(s.as_str()),
        other => Err(VmError::new(format!(
            "{fn_label} requires String, got {}",
            value_kind(other)
        ))),
    }
}

/// Convert a 4-bit nibble (0..=15) to its ASCII hex character.
///
/// Round 64 collapsed two near-identical helpers — `bytes::hex_char`
/// (lowercase) and `encoding::upper_hex` (uppercase) — that differed
/// only in the alphabetic case of the `'a'..='f'` digits. The two old
/// bodies are both implemented here so byte-identical output is
/// preserved at every existing call site.
///
/// `pub(crate)` so internal callers (and any future in-crate test) can
/// reference the helper. The integration-test lock in
/// `tests/builtin_nibble_to_hex_helper_tests.rs` proves the deletion was
/// a semantic no-op by driving both call paths through the public
/// builtin API (`bytes.to_hex` and `encoding.form_encode`).
pub(crate) fn nibble_to_hex(n: u8, uppercase: bool) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => {
            let base = if uppercase { b'A' } else { b'a' };
            (base + (n - 10)) as char
        }
        _ => unreachable!("nibble_to_hex called with n > 15"),
    }
}
