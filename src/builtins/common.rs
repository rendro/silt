//! Shared helper functions used by several builtin modules.
//!

use crate::typeinfo::bv;
use crate::value::Value;

pub(crate) fn ok(v: Value) -> Value {
    Value::variant(bv::OK, vec![v])
}

pub(super) fn err(s: impl Into<String>) -> Value {
    Value::variant(bv::ERR, vec![Value::String(s.into())])
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
/// (`tests/typecheck/round75_kind_naming_canonical_tests.rs`). `mod common` itself
/// remains private — only this single helper crosses the module wall.
pub(crate) fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
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
        Value::HostFn(_) => "HostFn",
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
/// `tests/lang/builtin_nibble_to_hex_helper_tests.rs` proves the deletion was
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
