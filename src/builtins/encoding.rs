//! `encoding.*` builtin functions: URL / percent encoding per RFC 3986.
//!
//! This module is deliberately narrow — it covers the piece that does
//! not belong in `bytes`. Base64 and hex encoding operate on
//! `Value::Bytes` and live in `src/builtins/bytes.rs`
//! (`bytes.to_base64` / `bytes.from_base64` / `bytes.to_hex` /
//! `bytes.from_hex`). Percent-encoding, by contrast, is a String ↔
//! String transform: the input is text that will end up in a URL
//! (query-string value, path segment, etc.), and the output is text.
//!
//! RFC 3986 §2.3 defines the "unreserved" character set as
//! `ALPHA / DIGIT / "-" / "." / "_" / "~"`. We encode every other byte
//! of the UTF-8 representation as `%HH` with upper-case hex, matching
//! the RFC's normalization recommendation (§6.2.2.1). Decoding is
//! case-insensitive on hex digits.
//!
//! `+` is treated as a literal `+` in both directions. The `+ ↔ space`
//! convention is specific to `application/x-www-form-urlencoded` (WHATWG
//! URL §form-urlencoded); it is intentionally not part of RFC 3986
//! percent-encoding and does not belong in this primitive. A future
//! `form` module can build on top of `encoding.url_encode` if needed.

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};

use crate::value::Value;
use crate::vm::{Vm, VmError};

/// The RFC 3986 unreserved set is `ALPHA / DIGIT / "-" / "." / "_" / "~"`.
/// `percent_encoding::NON_ALPHANUMERIC` already excludes alphanumerics,
/// so we take that set and add back `-`, `.`, `_`, `~` as *not* needing
/// encoding.
///
/// Built from `CONTROLS` to ensure every control byte is encoded, then
/// we explicitly add every non-unreserved printable ASCII byte. This is
/// the same construction pattern the `percent-encoding` crate docs
/// recommend when you want a strict RFC 3986 profile rather than the
/// looser `NON_ALPHANUMERIC` default.
const RFC3986_RESERVED: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'\x7f');

/// Dispatch `encoding.<name>(args)`.
pub fn call(_vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "url_encode" => url_encode(args),
        "url_decode" => url_decode(args),
        _ => Err(VmError::new(format!("unknown encoding function: {name}"))),
    }
}

// ── Helpers (mirror src/builtins/crypto.rs) ────────────────────────────

fn ok(v: Value) -> Value {
    Value::Variant("Ok".into(), vec![v])
}

fn err(s: impl Into<String>) -> Value {
    Value::Variant("Err".into(), vec![Value::String(s.into())])
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

// ── URL encoding ───────────────────────────────────────────────────────

fn url_encode(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("encoding.url_encode takes 1 argument".into()));
    }
    let s = require_string(&args[0], "encoding.url_encode")?;
    // `utf8_percent_encode` walks the UTF-8 bytes and replaces each byte
    // in `RFC3986_RESERVED` with its upper-case `%HH` form. The
    // `percent-encoding` crate always emits upper-case hex, which
    // satisfies our contract.
    let out: String = utf8_percent_encode(&s, RFC3986_RESERVED).collect();
    Ok(Value::String(out))
}

fn url_decode(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("encoding.url_decode takes 1 argument".into()));
    }
    let s = require_string(&args[0], "encoding.url_decode")?;

    // The `percent-encoding` crate's `percent_decode_str` is lenient:
    // a trailing or malformed `%` sequence is passed through verbatim.
    // That silently-accept-garbage behavior is wrong for a decoder:
    // callers need to know whether the input was well-formed. We do our
    // own strict scan first, then delegate the byte-level decoding to
    // the crate.
    if let Err(msg) = validate_percent_sequences(&s) {
        return Ok(err(msg));
    }

    let decoded_bytes = percent_decode_str(&s).collect::<Vec<u8>>();
    match String::from_utf8(decoded_bytes) {
        Ok(out) => Ok(ok(Value::String(out))),
        Err(_) => Ok(err("decoded bytes are not valid UTF-8")),
    }
}

/// Walk the input and require that every `%` is followed by two ASCII
/// hex digits. Returns `Ok(())` on well-formed input, `Err(msg)` on the
/// first malformed sequence.
fn validate_percent_sequences(s: &str) -> Result<(), String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // Need two more bytes, both ASCII hex digits.
            if i + 2 >= bytes.len() {
                return Err(format!("truncated percent-escape at offset {i}"));
            }
            let h1 = bytes[i + 1];
            let h2 = bytes[i + 2];
            if !is_ascii_hex_digit(h1) || !is_ascii_hex_digit(h2) {
                return Err(format!(
                    "invalid percent-escape `%{}{}` at offset {i}",
                    h1 as char, h2 as char
                ));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    Ok(())
}

fn is_ascii_hex_digit(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b)
}
