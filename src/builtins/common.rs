//! Shared helper functions used by several builtin modules.

use std::sync::Arc;

use crate::typeinfo::bv;
use crate::value::Value;

/// The most bytes one read of a connection takes (`tcp.read`, a chunk
/// of `stream.tcp_chunks`), and one read of a file of no known length
/// (`stream.file_chunks`): a caller may ask for more, and gets what one
/// read of this size gives. The buffer of a read is for what can
/// arrive, never for the number asked for.
pub(crate) const READ_AT_ONCE: usize = 64 * 1024;

pub(crate) fn ok(v: Value) -> Value {
    Value::variant(bv::OK, vec![v])
}

pub(super) fn err(s: impl Into<Arc<str>>) -> Value {
    Value::variant(bv::ERR, vec![Value::String(s.into())])
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
