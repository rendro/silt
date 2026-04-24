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

use std::sync::Arc;

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};

use super::common::{err, ok, require_string, value_kind};
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
        "form_encode" => form_encode(args),
        "form_decode" => form_decode(args),
        _ => Err(VmError::new(format!("unknown encoding function: {name}"))),
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

// ── Form encoding (application/x-www-form-urlencoded) ──────────────────
//
// `form_encode` / `form_decode` implement the
// `application/x-www-form-urlencoded` serialization used by HTML form
// submissions and most "classic" HTTP APIs. This sits on top of the
// RFC 3986 percent-encoder but with two form-specific conventions:
//
//  * `&` and `=` have structural meaning (pair separator / key-value
//    separator) so keys and values MUST encode them.
//  * `+` as a literal is reserved — this primitive percent-escapes it
//    on encode (`%2B`) and accepts both `+` (→ space) and `%2B` (→ `+`)
//    on decode, matching the WHATWG URL §form-urlencoded parser.
//
// We deliberately do NOT use `encoding.url_encode` here because that
// entrypoint is a pure RFC 3986 percent-encoder (it leaves `+` alone),
// and mixing the two would give the wrong behavior for either one.

/// Extract a `Vec<(String, String)>` from a `Value::List` of
/// `Value::Tuple([String, String])`. Returns a VmError if the shape
/// doesn't match — the typechecker normally rules this out, but the
/// dispatcher still runs even if type-checking was bypassed.
fn require_pair_list(arg: &Value, fn_label: &str) -> Result<Vec<(String, String)>, VmError> {
    let Value::List(items) = arg else {
        return Err(VmError::new(format!(
            "{fn_label} requires List((String, String)), got {}",
            value_kind(arg)
        )));
    };
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let Value::Tuple(parts) = item else {
            return Err(VmError::new(format!(
                "{fn_label}: element {i} is not a (String, String) tuple",
            )));
        };
        if parts.len() != 2 {
            return Err(VmError::new(format!(
                "{fn_label}: element {i} is a {}-tuple, need a 2-tuple",
                parts.len()
            )));
        }
        let (Value::String(k), Value::String(v)) = (&parts[0], &parts[1]) else {
            return Err(VmError::new(format!(
                "{fn_label}: element {i} is not a (String, String) tuple",
            )));
        };
        out.push((k.clone(), v.clone()));
    }
    Ok(out)
}

/// Percent-escape a single form component (key or value) using the
/// WHATWG form-urlencoded byte set. Any byte outside
/// `ALPHA / DIGIT / "*" / "-" / "." / "_"` is either converted to `+`
/// (for space) or emitted as `%HH`.
fn form_escape_component(s: &str) -> String {
    // Capacity hint: most inputs don't need escaping, so start from the
    // byte length and let String grow if we're wrong.
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b;
        if c == b' ' {
            out.push('+');
        } else if c.is_ascii_alphanumeric() || matches!(c, b'*' | b'-' | b'.' | b'_') {
            out.push(c as char);
        } else {
            // Upper-case hex, matching url_encode's output style.
            out.push('%');
            out.push(upper_hex(c >> 4));
            out.push(upper_hex(c & 0x0F));
        }
    }
    out
}

fn upper_hex(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'A' + (nibble - 10)) as char,
        _ => unreachable!("nibble must be in 0..=15"),
    }
}

fn form_encode(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("encoding.form_encode takes 1 argument".into()));
    }
    let pairs = require_pair_list(&args[0], "encoding.form_encode")?;
    if pairs.is_empty() {
        return Ok(Value::String(String::new()));
    }
    // Join `key=value` segments with `&`. We write into one allocation
    // and don't bother with `Vec<String>` + `join("&")` because the
    // escaping step already forces per-pair allocation.
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&form_escape_component(k));
        out.push('=');
        out.push_str(&form_escape_component(v));
    }
    Ok(Value::String(out))
}

/// Decode a single form component: `+` → space, `%HH` → byte HH.
/// Returns `Err(msg)` on malformed percent escapes or non-UTF-8 output.
pub(crate) fn form_decode_component(s: &str) -> Result<String, String> {
    // Translate `+` to space before feeding the buffer to the
    // percent-decoder. Doing it in one pass keeps the validation /
    // decoding responsibilities separate from the +/space convention.
    let mut plus_translated = Vec::with_capacity(s.len());
    for b in s.as_bytes() {
        plus_translated.push(if *b == b'+' { b' ' } else { *b });
    }
    // SAFETY: `s` was valid UTF-8 and `+` is U+002B, `space` is U+0020 —
    // both single-byte ASCII, so swapping preserves UTF-8 validity.
    let translated =
        std::str::from_utf8(&plus_translated).expect("ASCII-level substitution preserves UTF-8");
    // Percent validation up front (same strict policy as url_decode) so
    // callers don't get lenient passthrough of malformed input.
    validate_percent_sequences(translated)?;
    let decoded = percent_decode_str(translated).collect::<Vec<u8>>();
    String::from_utf8(decoded).map_err(|_| "decoded bytes are not valid UTF-8".to_string())
}

fn form_decode(args: &[Value]) -> Result<Value, VmError> {
    if args.len() != 1 {
        return Err(VmError::new("encoding.form_decode takes 1 argument".into()));
    }
    let body = require_string(&args[0], "encoding.form_decode")?;
    if body.is_empty() {
        return Ok(ok(Value::List(Arc::new(Vec::new()))));
    }
    let mut out: Vec<Value> = Vec::new();
    for (i, segment) in body.split('&').enumerate() {
        // An empty segment (e.g. leading `&` or `&&`) is silently
        // skipped. The WHATWG parser specifies the same behavior: "if
        // bytes is the empty byte sequence, then continue". It's
        // common in real-world requests and not a useful error case.
        if segment.is_empty() {
            continue;
        }
        // Split on the FIRST `=` only. A value that legitimately
        // contains `=` will have it percent-escaped by any conforming
        // encoder, but we still need to not break on the raw byte.
        let (raw_key, raw_val) = match segment.find('=') {
            Some(pos) => (&segment[..pos], &segment[pos + 1..]),
            // A key with no `=` means "empty value", not "malformed".
            // HTML forms with a checkbox whose value is "" produce
            // exactly this shape.
            None => (segment, ""),
        };
        let key = match form_decode_component(raw_key) {
            Ok(k) => k,
            Err(msg) => {
                return Ok(err(format!("pair {i}: key: {msg}")));
            }
        };
        let val = match form_decode_component(raw_val) {
            Ok(v) => v,
            Err(msg) => {
                return Ok(err(format!("pair {i}: value: {msg}")));
            }
        };
        out.push(Value::Tuple(vec![Value::String(key), Value::String(val)]));
    }
    Ok(ok(Value::List(Arc::new(out))))
}
