//! Shared helper functions used by several builtin modules.

use crate::typeinfo::bv;
use crate::value::{MAX_COLLECTION_LEN, Value};
use crate::vm::VmError;

/// The most bytes one read of a connection takes (`tcp.read`, a chunk
/// of `stream.tcp_chunks`), and one read of a file of no known length
/// (`stream.file_chunks`): a caller may ask for more, and gets what one
/// read of this size gives. The buffer of a read is for what can
/// arrive, never for the number asked for.
pub(crate) const READ_AT_ONCE: usize = 64 * 1024;

/// A text that a builtin writes, which holds at most
/// [`MAX_COLLECTION_LEN`] bytes: a write that would make it longer
/// fails, and the text says how long it would have been at least. As
/// [`std::fmt::Write`] for what a formatter writes, as
/// [`std::io::Write`] for an encoder that writes bytes (which must be
/// UTF-8).
#[derive(Default)]
pub(crate) struct Bounded {
    bytes: Vec<u8>,
    over: Option<usize>,
}

impl Bounded {
    fn take(&mut self, part: &[u8]) -> bool {
        let len = self.bytes.len().saturating_add(part.len());
        if len > MAX_COLLECTION_LEN {
            self.over = Some(len);
            return false;
        }
        self.bytes.extend_from_slice(part);
        true
    }

    /// The text, or the error `name` gives for one too long: what was
    /// written, if no write failed, or the failure `failed` is (the
    /// writer's own, when it was not the length).
    pub(crate) fn text(self, name: &str, failed: Option<String>) -> Result<String, VmError> {
        if let Some(len) = self.over {
            return Err(too_long_text(name, len));
        }
        if let Some(failure) = failed {
            return Err(VmError::new(failure));
        }
        String::from_utf8(self.bytes).map_err(|_| {
            VmError::new(format!(
                "{name}: internal error: the text written is not UTF-8"
            ))
        })
    }
}

impl std::fmt::Write for Bounded {
    fn write_str(&mut self, part: &str) -> std::fmt::Result {
        match self.take(part.as_bytes()) {
            true => Ok(()),
            false => Err(std::fmt::Error),
        }
    }
}

impl std::io::Write for Bounded {
    fn write(&mut self, part: &[u8]) -> std::io::Result<usize> {
        match self.take(part) {
            true => Ok(part.len()),
            false => Err(std::io::Error::other("the text is too long")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What `name` says of a string of `len` bytes, more than a string may
/// hold: `<name>: result would exceed maximum string size (N bytes >
/// 10000000 limit)`. (Where the text was stopped as it was written,
/// `len` is as far as it had come: it would have been longer.)
pub(crate) fn too_long_text(name: &str, len: usize) -> VmError {
    VmError::new(format!(
        "{name}: result would exceed maximum string size ({len} bytes > {MAX_COLLECTION_LEN} limit)"
    ))
}

pub(crate) fn ok(v: Value) -> Value {
    Value::variant(bv::OK, vec![v])
}

/// Convert a 4-bit nibble (0..=15) to its ASCII hex character.
///
/// It replaces two near-identical helpers — `bytes::hex_char`
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
