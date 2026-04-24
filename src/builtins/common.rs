//! Shared helper functions used by several builtin modules.
//!
//! Round 57 collapsed four copy-pasted sets of helpers (`ok`, `err`,
//! `require_string`, `require_int`, `require_bytes`, `value_kind`) that
//! lived verbatim in `uuid.rs`, `encoding.rs`, `crypto.rs`, and
//! `bytes.rs`. Each call site had the same bodies and the same error
//! phrasing, so we hoist one canonical copy here.
//!
//! `stream.rs` and `tcp.rs` keep their own local helpers: their
//! signatures differ (they return `&str` rather than `String` from
//! `require_string`, omit `value_kind` from error messages, and wrap
//! `err` in module-specific error variants like `TcpUnknown`).

use std::sync::Arc;

use crate::value::Value;
use crate::vm::VmError;

pub(super) fn ok(v: Value) -> Value {
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

pub(super) fn value_kind(v: &Value) -> &'static str {
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
