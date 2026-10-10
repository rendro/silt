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
use crate::value::Value;
use crate::vm::VmError;

/// Dispatch the builtin `trait Error for BytesError` method table.
/// Scaffolding lives in `super::dispatch_error_trait`; this site just
/// supplies the variant → message rendering.
pub fn call_bytes_error_trait(name: &str, args: &[Value]) -> Result<Value, VmError> {
    super::dispatch_error_trait("BytesError", name, args, |tag, fields| {
        Some(match (tag, fields) {
            ("BytesInvalidUtf8", [Value::Int(offset)]) => {
                format!("invalid UTF-8 at byte {offset}")
            }
            ("BytesInvalidHex", [Value::String(m)]) => format!("invalid hex: {m}"),
            ("BytesInvalidBase64", [Value::String(m)]) => {
                format!("invalid base64: {m}")
            }
            ("BytesByteOutOfRange", [Value::Int(v)]) => {
                format!("byte value out of range (expected 0..=255): {v}")
            }
            ("BytesOutOfBounds", [Value::Int(idx)]) => {
                format!("index out of bounds: {idx}")
            }
            _ => return None,
        })
    })
}

// ── Helpers ────────────────────────────────────────────────────────────

fn bytes_err(variant: Value) -> Value {
    Value::variant(bv::ERR, vec![variant])
}

fn err_utf8(offset: usize) -> Value {
    bytes_err(Value::variant(
        bv::BYTES_INVALID_UTF8,
        vec![Value::Int(offset as i64)],
    ))
}

fn err_hex(msg: impl Into<String>) -> Value {
    bytes_err(Value::variant(
        bv::BYTES_INVALID_HEX,
        vec![Value::String(msg.into())],
    ))
}

fn err_base64(msg: impl Into<String>) -> Value {
    bytes_err(Value::variant(
        bv::BYTES_INVALID_BASE64,
        vec![Value::String(msg.into())],
    ))
}

fn err_byte_range(value: i64) -> Value {
    bytes_err(Value::variant(
        bv::BYTES_BYTE_OUT_OF_RANGE,
        vec![Value::Int(value)],
    ))
}

fn err_oob(idx: i64) -> Value {
    bytes_err(Value::variant(
        bv::BYTES_OUT_OF_BOUNDS,
        vec![Value::Int(idx)],
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
            Ok(s) => ok(Value::String(s.to_string())),
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
        let mut out = Vec::new();
        for item in xs.to_vec()? {
            let Value::Int(n) = item else {
                return Err(unsound("bytes.from_list"));
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

    fn slice(b: Bytes, start: i64, end: i64) -> Value {
        let Ok(from) = usize::try_from(start) else {
            return err_oob(start);
        };
        let Ok(to) = usize::try_from(end) else {
            return err_oob(end);
        };
        if from > to {
            return err_oob(start);
        }
        match b.get(from..to) {
            Some(part) => ok(bytes(part.to_vec())),
            None => err_oob(end),
        }
    }

    fn concat(a: Bytes, b: Bytes) -> Vec<u8> {
        [a.as_slice(), b.as_slice()].concat()
    }

    fn concat_all(parts: List) -> Result<Vec<u8>, VmError> {
        let mut out = Vec::new();
        for part in parts.to_vec()? {
            let Value::Bytes(part) = part else {
                return Err(unsound("bytes.concat_all"));
            };
            out.extend_from_slice(&part);
        }
        Ok(out)
    }

    fn get(b: Bytes, i: i64) -> Value {
        match usize::try_from(i).ok().and_then(|at| b.get(at)) {
            Some(byte) => ok(Value::Int(*byte as i64)),
            None => err_oob(i),
        }
    }

    fn eq(a: Bytes, b: Bytes) -> bool {
        a == b
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

    fn split(b: Bytes, sep: Bytes) -> Result<Vec<Value>, VmError> {
        if sep.is_empty() {
            return Err(VmError::new(
                "bytes.split: separator must be non-empty".into(),
            ));
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
        Ok(parts)
    }
}
