//! `bytes.*` builtin functions: immutable byte sequences with structural
//! equality. The value variant `Value::Bytes(Arc<Vec<u8>>)` is defined in
//! `src/value.rs`; this module exposes the user-facing operations.
//!
//! All functions are pure (no I/O) — no scheduler integration needed. The
//! tcp module (PR 2) will use `Value::Bytes` as its read/write payload type.
//!
//! Forward-compat: when `Bytes` is later promoted to a language-level
//! `Type::Bytes`, every function here remains valid; method-form dispatch
//! (`b.length()` → `bytes.length(b)`) is added on top via traits.

use std::sync::Arc;

use base64::Engine;

use crate::value::Value;
use crate::vm::{Vm, VmError};

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

/// Dispatch `bytes.<name>(args)`.
pub fn call(_vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "empty" => empty(args),
        "from_string" => from_string(args),
        "to_string" => to_string(args),
        "from_hex" => from_hex(args),
        "to_hex" => to_hex(args),
        "from_base64" => from_base64(args),
        "to_base64" => to_base64(args),
        "from_list" => from_list(args),
        "to_list" => to_list(args),
        "length" => length(args),
        "slice" => slice(args),
        "concat" => concat(args),
        "concat_all" => concat_all(args),
        "get" => get(args),
        "eq" => eq(args),
        "index_of" => index_of(args),
        "starts_with" => starts_with(args),
        "ends_with" => ends_with(args),
        "split" => split(args),
        _ => Err(VmError::new(format!("unknown bytes function: {name}"))),
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn ok(v: Value) -> Value {
    Value::Variant("Ok".into(), vec![v])
}

fn bytes_err(variant: Value) -> Value {
    Value::Variant("Err".into(), vec![variant])
}

fn err_utf8(offset: usize) -> Value {
    bytes_err(Value::Variant(
        "BytesInvalidUtf8".into(),
        vec![Value::Int(offset as i64)],
    ))
}

fn err_hex(msg: impl Into<String>) -> Value {
    bytes_err(Value::Variant(
        "BytesInvalidHex".into(),
        vec![Value::String(msg.into())],
    ))
}

fn err_base64(msg: impl Into<String>) -> Value {
    bytes_err(Value::Variant(
        "BytesInvalidBase64".into(),
        vec![Value::String(msg.into())],
    ))
}

fn err_byte_range(value: i64) -> Value {
    bytes_err(Value::Variant(
        "BytesByteOutOfRange".into(),
        vec![Value::Int(value)],
    ))
}

fn err_oob(idx: i64) -> Value {
    bytes_err(Value::Variant(
        "BytesOutOfBounds".into(),
        vec![Value::Int(idx)],
    ))
}

fn require_bytes(arg: &Value, fn_label: &str) -> Result<Arc<Vec<u8>>, VmError> {
    match arg {
        Value::Bytes(b) => Ok(b.clone()),
        other => Err(VmError::new(format!(
            "{fn_label} requires Bytes, got {}",
            value_kind(other)
        ))),
    }
}

fn require_string(arg: &Value, fn_label: &str) -> Result<String, VmError> {
    match arg {
        Value::String(s) => Ok(s.clone()),
        other => Err(VmError::new(format!(
            "{fn_label} requires String, got {}",
            value_kind(other)
        ))),
    }
}

fn require_int(arg: &Value, fn_label: &str) -> Result<i64, VmError> {
    match arg {
        Value::Int(n) => Ok(*n),
        other => Err(VmError::new(format!(
            "{fn_label} requires Int, got {}",
            value_kind(other)
        ))),
    }
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "Int",
        Value::Float(_) => "Float",
        Value::ExtFloat(_) => "ExtFloat",
        Value::Bool(_) => "Bool",
        Value::String(_) => "String",
        Value::List(_) => "List",
        Value::Bytes(_) => "Bytes",
        Value::Tuple(_) => "Tuple",
        _ => "value",
    }
}

// ── Constructors ───────────────────────────────────────────────────────

fn empty(args: &[Value]) -> Result<Value, VmError> {
    if !args.is_empty() {
        return Err(VmError::new("bytes.empty takes 0 arguments".into()));
    }
    Ok(Value::Bytes(Arc::new(Vec::new())))
}

fn from_string(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("bytes.from_string takes 1 argument".into()));
    }
    let s = require_string(&args[0], "bytes.from_string")?;
    Ok(Value::Bytes(Arc::new(s.into_bytes())))
}

fn to_string(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("bytes.to_string takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "bytes.to_string")?;
    match std::str::from_utf8(&b) {
        Ok(s) => Ok(ok(Value::String(s.to_string()))),
        Err(e) => Ok(err_utf8(e.valid_up_to())),
    }
}

fn from_hex(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("bytes.from_hex takes 1 argument".into()));
    }
    let s = require_string(&args[0], "bytes.from_hex")?;
    if s.len() % 2 != 0 {
        return Ok(err_hex(format!(
            "hex string must have even length, got {} chars",
            s.len()
        )));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = match hex_nibble(bytes[i]) {
            Some(n) => n,
            None => {
                return Ok(err_hex(format!(
                    "invalid hex character at position {i}: {:?}",
                    bytes[i] as char
                )));
            }
        };
        let lo = match hex_nibble(bytes[i + 1]) {
            Some(n) => n,
            None => {
                return Ok(err_hex(format!(
                    "invalid hex character at position {}: {:?}",
                    i + 1,
                    bytes[i + 1] as char
                )));
            }
        };
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(ok(Value::Bytes(Arc::new(out))))
}

fn to_hex(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("bytes.to_hex takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "bytes.to_hex")?;
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b.iter() {
        s.push(hex_char(byte >> 4));
        s.push(hex_char(byte & 0x0f));
    }
    Ok(Value::String(s))
}

fn from_base64(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("bytes.from_base64 takes 1 argument".into()));
    }
    let s = require_string(&args[0], "bytes.from_base64")?;
    match base64::engine::general_purpose::STANDARD.decode(s.as_bytes()) {
        Ok(decoded) => Ok(ok(Value::Bytes(Arc::new(decoded)))),
        Err(e) => Ok(err_base64(e.to_string())),
    }
}

fn to_base64(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("bytes.to_base64 takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "bytes.to_base64")?;
    Ok(Value::String(
        base64::engine::general_purpose::STANDARD.encode(b.as_slice()),
    ))
}

fn from_list(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("bytes.from_list takes 1 argument".into()));
    }
    let Value::List(xs) = &args[0] else {
        return Err(VmError::new(format!(
            "bytes.from_list requires List(Int), got {}",
            value_kind(&args[0])
        )));
    };
    let mut out = Vec::with_capacity(xs.len());
    for (i, v) in xs.iter().enumerate() {
        match v {
            Value::Int(n) if *n >= 0 && *n <= 255 => out.push(*n as u8),
            Value::Int(n) => {
                return Ok(err_byte_range(*n));
            }
            other => {
                return Err(VmError::new(format!(
                    "bytes.from_list element at position {i} is not Int: {}",
                    value_kind(other)
                )));
            }
        }
    }
    Ok(ok(Value::Bytes(Arc::new(out))))
}

fn to_list(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("bytes.to_list takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "bytes.to_list")?;
    let items: Vec<Value> = b.iter().map(|&byte| Value::Int(byte as i64)).collect();
    Ok(Value::List(Arc::new(items)))
}

// ── Accessors ──────────────────────────────────────────────────────────

fn length(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("bytes.length takes 1 argument".into()));
    }
    let b = require_bytes(&args[0], "bytes.length")?;
    Ok(Value::Int(b.len() as i64))
}

fn slice(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 3 {
        return Err(VmError::new(
            "bytes.slice takes 3 arguments (bytes, start, end)".into(),
        ));
    }
    let b = require_bytes(&args[0], "bytes.slice")?;
    let start = require_int(&args[1], "bytes.slice")?;
    let end = require_int(&args[2], "bytes.slice")?;
    if start < 0 {
        return Ok(err_oob(start));
    }
    if end < 0 {
        return Ok(err_oob(end));
    }
    let start_u = start as usize;
    let end_u = end as usize;
    if start_u > end_u {
        return Ok(err_oob(start));
    }
    if end_u > b.len() {
        return Ok(err_oob(end));
    }
    Ok(ok(Value::Bytes(Arc::new(b[start_u..end_u].to_vec()))))
}

fn concat(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("bytes.concat takes 2 arguments".into()));
    }
    let a = require_bytes(&args[0], "bytes.concat")?;
    let b = require_bytes(&args[1], "bytes.concat")?;
    let mut out = Vec::with_capacity(a.len() + b.len());
    out.extend_from_slice(&a);
    out.extend_from_slice(&b);
    Ok(Value::Bytes(Arc::new(out)))
}

fn concat_all(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("bytes.concat_all takes 1 argument".into()));
    }
    let Value::List(xs) = &args[0] else {
        return Err(VmError::new(format!(
            "bytes.concat_all requires List(Bytes), got {}",
            value_kind(&args[0])
        )));
    };
    let mut total = 0;
    for (i, v) in xs.iter().enumerate() {
        match v {
            Value::Bytes(b) => total += b.len(),
            other => {
                return Err(VmError::new(format!(
                    "bytes.concat_all element at position {i} is not Bytes: {}",
                    value_kind(other)
                )));
            }
        }
    }
    let mut out = Vec::with_capacity(total);
    for v in xs.iter() {
        if let Value::Bytes(b) = v {
            out.extend_from_slice(b);
        }
    }
    Ok(Value::Bytes(Arc::new(out)))
}

fn get(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("bytes.get takes 2 arguments".into()));
    }
    let b = require_bytes(&args[0], "bytes.get")?;
    let i = require_int(&args[1], "bytes.get")?;
    if i < 0 {
        return Ok(err_oob(i));
    }
    let idx = i as usize;
    if idx >= b.len() {
        return Ok(err_oob(i));
    }
    Ok(ok(Value::Int(b[idx] as i64)))
}

fn eq(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("bytes.eq takes 2 arguments".into()));
    }
    let a = require_bytes(&args[0], "bytes.eq")?;
    let b = require_bytes(&args[1], "bytes.eq")?;
    Ok(Value::Bool(a == b))
}

// ── Search / prefix / suffix / split ──────────────────────────────────

/// Find the byte offset of the first occurrence of `needle` in `hay`.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > hay.len() {
        return None;
    }
    // Simple linear scan — hay.len() small in practice and avoids a
    // dependency on memchr. Callers with large buffers can layer their
    // own optimized search on top.
    let last = hay.len() - needle.len();
    let mut i = 0;
    while i <= last {
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn index_of(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("bytes.index_of takes 2 arguments".into()));
    }
    let b = require_bytes(&args[0], "bytes.index_of")?;
    let needle = require_bytes(&args[1], "bytes.index_of")?;
    match find_subslice(&b, &needle) {
        Some(i) => Ok(Value::Variant("Some".into(), vec![Value::Int(i as i64)])),
        None => Ok(Value::Variant("None".into(), Vec::new())),
    }
}

fn starts_with(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("bytes.starts_with takes 2 arguments".into()));
    }
    let b = require_bytes(&args[0], "bytes.starts_with")?;
    let prefix = require_bytes(&args[1], "bytes.starts_with")?;
    Ok(Value::Bool(b.starts_with(&prefix)))
}

fn ends_with(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("bytes.ends_with takes 2 arguments".into()));
    }
    let b = require_bytes(&args[0], "bytes.ends_with")?;
    let suffix = require_bytes(&args[1], "bytes.ends_with")?;
    Ok(Value::Bool(b.ends_with(&suffix)))
}

fn split(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 2 {
        return Err(VmError::new("bytes.split takes 2 arguments".into()));
    }
    let b = require_bytes(&args[0], "bytes.split")?;
    let sep = require_bytes(&args[1], "bytes.split")?;
    if sep.is_empty() {
        return Err(VmError::new(
            "bytes.split: separator must be non-empty".into(),
        ));
    }
    // Mirror Rust's `str::split` / silt's `string.split` on empty input:
    // splitting an empty `b` yields a list with a single empty-bytes element.
    let mut parts: Vec<Value> = Vec::new();
    let mut start = 0usize;
    while start <= b.len() {
        match find_subslice(&b[start..], &sep) {
            Some(rel) => {
                let i = start + rel;
                parts.push(Value::Bytes(Arc::new(b[start..i].to_vec())));
                start = i + sep.len();
            }
            None => {
                parts.push(Value::Bytes(Arc::new(b[start..].to_vec())));
                break;
            }
        }
    }
    Ok(Value::List(Arc::new(parts)))
}

// ── Hex digit helpers ──────────────────────────────────────────────────

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn hex_char(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + n - 10) as char,
        _ => unreachable!("hex_char called with n > 15"),
    }
}
