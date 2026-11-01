//! `bytes.*` builtin functions: immutable byte sequences with structural
//! equality. The value variant `Value::Bytes(Arc<Vec<u8>>)` is defined in
//! `src/value/mod.rs`; this module exposes the user-facing operations.
//!
//! All functions are pure (no I/O) — no scheduler integration needed. The
//! tcp module (PR 2) will use `Value::Bytes` as its read/write payload type.
//!
//! Forward-compat: when `Bytes` is later promoted to a language-level
//! `Type::Bytes`, every function here remains valid; method-form dispatch
//! (`b.length()` → `bytes.length(b)`) is added on top via traits.

use std::sync::Arc;

use base64::Engine;

use super::common::{nibble_to_hex, ok};
use super::typed::{Bytes, List, builtins, unsound};
use crate::typeinfo::bv;
use crate::value::{MAX_COLLECTION_LEN, Value};
use crate::vm::VmError;

/// Refuses Bytes of `size` that `name` would make: more than a list
/// may hold elements ([`MAX_COLLECTION_LEN`]).
fn fits(name: &str, size: usize) -> Result<(), VmError> {
    match size > MAX_COLLECTION_LEN {
        true => Err(VmError::new(format!(
            "{name}: result would exceed maximum size ({size} bytes > {MAX_COLLECTION_LEN} limit)"
        ))),
        false => Ok(()),
    }
}

/// What `BytesError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("InvalidUtf8", [Value::Int(offset)]) => {
            format!("invalid UTF-8 at byte {offset}")
        }
        ("InvalidHex", [Value::String(m)]) => format!("invalid hex: {m}"),
        ("InvalidBase64", [Value::String(m)]) => {
            format!("invalid base64: {m}")
        }
        ("ByteOutOfRange", [Value::Int(v)]) => {
            format!("byte value out of range (expected 0..=255): {v}")
        }
        ("Other", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

// ── Helpers ────────────────────────────────────────────────────────────

fn bytes_err(variant: Value) -> Value {
    Value::variant(bv::ERR, [variant])
}

fn err_utf8(offset: usize) -> Value {
    bytes_err(Value::variant(
        bv::BYTES_INVALID_UTF8,
        [Value::Int(offset as i64)],
    ))
}

fn err_hex(msg: impl Into<Arc<str>>) -> Value {
    bytes_err(Value::variant(
        bv::BYTES_INVALID_HEX,
        [Value::String(msg.into())],
    ))
}

fn err_base64(msg: impl Into<Arc<str>>) -> Value {
    bytes_err(Value::variant(
        bv::BYTES_INVALID_BASE64,
        [Value::String(msg.into())],
    ))
}

fn err_byte_range(value: i64) -> Value {
    bytes_err(Value::variant(
        bv::BYTES_BYTE_OUT_OF_RANGE,
        [Value::Int(value)],
    ))
}

/// The value of an ASCII hex digit.
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Find the byte offset of the first occurrence of `needle` in `hay`.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    // Simple linear scan — hay.len() small in practice and avoids a
    // dependency on memchr. Callers with large buffers can layer their
    // own optimized search on top.
    hay.windows(needle.len())
        .position(|window| window == needle)
}

fn bytes(bytes: Vec<u8>) -> Value {
    Value::Bytes(Arc::new(bytes))
}

builtins! {
    // ── Constructors ───────────────────────────────────────────────────

    fn empty() -> Vec<u8> {
        Vec::new()
    }

    fn from_string(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    fn to_string(b: Bytes) -> Value {
        match std::str::from_utf8(b) {
            Ok(s) => ok(Value::String(s.into())),
            Err(e) => err_utf8(e.valid_up_to()),
        }
    }

    fn from_hex(s: &str) -> Value {
        if !s.len().is_multiple_of(2) {
            return err_hex(format!(
                "hex string must have even length, got {} chars",
                s.len()
            ));
        }
        let mut out = Vec::with_capacity(s.len() / 2);
        let mut value = 0;
        for (at, digit) in s.bytes().enumerate() {
            let Some(nibble) = hex_nibble(digit) else {
                return err_hex(format!(
                    "invalid hex character at position {at}: {:?}",
                    digit as char
                ));
            };
            value = (value << 4) | nibble;
            if at % 2 == 1 {
                out.push(value);
            }
        }
        ok(bytes(out))
    }

    fn to_hex(b: Bytes) -> String {
        let mut s = String::with_capacity(b.len() * 2);
        for byte in b.iter() {
            s.push(nibble_to_hex(byte >> 4, false));
            s.push(nibble_to_hex(byte & 0x0f, false));
        }
        s
    }

    fn from_base64(s: &str) -> Value {
        match base64::engine::general_purpose::STANDARD.decode(s.as_bytes()) {
            Ok(decoded) => ok(bytes(decoded)),
            Err(e) => err_base64(e.to_string()),
        }
    }

    fn to_base64(b: Bytes) -> String {
        base64::engine::general_purpose::STANDARD.encode(b.as_slice())
    }

    fn from_list(xs: List) -> Result<Value, VmError> {
        xs.writable()?;
        let mut out = Vec::new();
        for item in xs.iter() {
            let Value::Int(n) = item else {
                return Err(unsound("bytes.from_list", "xs"));
            };
            match u8::try_from(n) {
                Ok(byte) => out.push(byte),
                Err(_) => return Ok(err_byte_range(n)),
            }
        }
        Ok(ok(bytes(out)))
    }

    fn to_list(b: Bytes) -> Vec<Value> {
        b.iter().map(|&byte| Value::Int(byte as i64)).collect()
    }

    // ── Accessors ──────────────────────────────────────────────────────

    fn length(b: Bytes) -> i64 {
        b.len() as i64
    }

    // As `string.slice`: an index past the end is the end, a start
    // past the end gives nothing, and a negative index is an error.
    fn slice(b: Bytes, start: i64, end: i64) -> Result<Vec<u8>, VmError> {
        if let Some(negative) = [start, end].into_iter().find(|index| *index < 0) {
            return Err(VmError::new(format!(
                "bytes.slice: negative index {negative}"
            )));
        }
        let start = (start as usize).min(b.len());
        let end = (end as usize).min(b.len());
        Ok(match start > end {
            true => Vec::new(),
            false => b[start..end].to_vec(),
        })
    }

    fn concat(a: Bytes, b: Bytes) -> Result<Vec<u8>, VmError> {
        fits("bytes.concat", a.len().saturating_add(b.len()))?;
        Ok([a.as_slice(), b.as_slice()].concat())
    }

    // The size is counted before anything is copied: the parts can be
    // one value many times.
    fn concat_all(parts: List) -> Result<Vec<u8>, VmError> {
        parts.writable()?;
        let mut size: usize = 0;
        for part in parts.iter() {
            let Value::Bytes(part) = part else {
                return Err(unsound("bytes.concat_all", "parts"));
            };
            size = size.saturating_add(part.len());
        }
        fits("bytes.concat_all", size)?;
        let mut out = Vec::with_capacity(size);
        for part in parts.iter() {
            if let Value::Bytes(part) = part {
                out.extend_from_slice(&part);
            }
        }
        Ok(out)
    }

    fn get(b: Bytes, i: i64) -> Option<Value> {
        let byte = b.get(usize::try_from(i).ok()?)?;
        Some(Value::Int(*byte as i64))
    }

    // ── Search / prefix / suffix / split ──────────────────────────────

    fn index_of(b: Bytes, needle: Bytes) -> Option<Value> {
        find_subslice(b, needle).map(|at| Value::Int(at as i64))
    }

    fn starts_with(b: Bytes, prefix: Bytes) -> bool {
        b.starts_with(prefix)
    }

    fn ends_with(b: Bytes, suffix: Bytes) -> bool {
        b.ends_with(suffix)
    }

    fn split(b: Bytes, sep: Bytes) -> Vec<Value> {
        // An empty separator splits into the single bytes, with no
        // empty piece at either end, as `string.split` splits a string
        // into its characters.
        if sep.is_empty() {
            return b.as_slice().iter().map(|byte| bytes(vec![*byte])).collect();
        }
        // Mirror Rust's `str::split` / silt's `string.split` on empty input:
        // splitting an empty `b` yields a list with a single empty-bytes element.
        let mut parts: Vec<Value> = Vec::new();
        let mut rest = b.as_slice();
        while let Some(at) = find_subslice(rest, sep) {
            parts.push(bytes(rest[..at].to_vec()));
            rest = &rest[at + sep.len()..];
        }
        parts.push(bytes(rest.to_vec()));
        parts
    }
}
