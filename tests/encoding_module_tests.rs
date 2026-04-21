//! End-to-end tests for the `encoding` builtin module.
//!
//! The surface is narrow (two functions), so this file focuses on:
//! - Correctness of the RFC 3986 unreserved set (passthrough).
//! - Correctness of the encoder on reserved ASCII, spaces, and multi-byte UTF-8.
//! - Correct failure modes on the decoder (truncated `%`, non-hex digits,
//!   invalid UTF-8 after decoding).
//! - The `+` literal invariant (NOT decoded as space — that's form-decoding).
//! - Round-trip `url_decode(url_encode(s)) == Ok(s)` over a grab-bag of
//!   tricky strings (emoji, quotes, newlines, nulls).
//! - Doc ↔ registration cross-check mirroring the crypto module's test.

use std::path::Path;
use std::sync::Arc;

use silt::types::Severity;
use silt::value::Value;

fn run(input: &str) -> Value {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = silt::compiler::Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = silt::vm::Vm::new();
    vm.run(script).expect("runtime error")
}

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = silt::typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

fn expect_string(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => panic!("expected String, got {other:?}"),
    }
}

// ── url_encode ─────────────────────────────────────────────────────────

/// The RFC 3986 unreserved set must pass through verbatim.
/// ALPHA / DIGIT / "-" / "." / "_" / "~" — none of these ever need encoding.
#[test]
fn test_url_encode_unreserved_set_is_identity() {
    let input = "abc-._~0-9XYZ";
    let v = run(&format!(
        r#"
import encoding
fn main() {{
  encoding.url_encode("{input}")
}}
"#
    ));
    assert_eq!(expect_string(v), input);
}

/// Space must encode as `%20`, never `+`. `+ ↔ space` is form-encoding
/// (application/x-www-form-urlencoded), not RFC 3986 percent-encoding.
#[test]
fn test_url_encode_space_is_percent_twenty_not_plus() {
    let v = run(
        r#"
import encoding
fn main() {
  encoding.url_encode("hello world")
}
"#,
    );
    let s = expect_string(v);
    assert_eq!(s, "hello%20world");
    assert!(!s.contains('+'), "space must not encode as `+`, got: {s}");
}

/// Reserved ASCII characters that commonly need escaping in query
/// strings should become their exact `%HH` forms.
#[test]
fn test_url_encode_reserved_query_chars() {
    let v = run(
        r#"
import encoding
fn main() {
  encoding.url_encode("a&b=c?d#e")
}
"#,
    );
    let s = expect_string(v);
    // Don't pin the full output — just assert the reserved bytes are
    // escaped. Alpha passes through; `&`, `=`, `?`, `#` must not.
    assert!(s.contains("%26"), "missing %26 for &, got: {s}");
    assert!(s.contains("%3D"), "missing %3D for =, got: {s}");
    assert!(s.contains("%3F"), "missing %3F for ?, got: {s}");
    assert!(s.contains("%23"), "missing %23 for #, got: {s}");
}

/// Non-ASCII input encodes its UTF-8 bytes. "é" is U+00E9, UTF-8 `C3 A9`.
#[test]
fn test_url_encode_non_ascii_encodes_utf8_bytes() {
    let v = run(
        r#"
import encoding
fn main() {
  encoding.url_encode("café")
}
"#,
    );
    assert_eq!(expect_string(v), "caf%C3%A9");
}

/// The encoder emits upper-case hex as a matter of convention (RFC 3986
/// §6.2.2.1). Decoders must accept either case.
#[test]
fn test_url_encode_emits_upper_case_hex() {
    let v = run(
        r#"
import encoding
fn main() {
  encoding.url_encode("/")
}
"#,
    );
    assert_eq!(expect_string(v), "%2F");
}

// ── url_decode ─────────────────────────────────────────────────────────

/// Decoder must accept both `%2F` and `%2f`.
#[test]
fn test_url_decode_accepts_lower_and_upper_case_hex() {
    let v_upper = run(
        r#"
import encoding
fn main() {
  match encoding.url_decode("%2F") {
    Ok(s) -> s
    Err(e) -> e
  }
}
"#,
    );
    assert_eq!(expect_string(v_upper), "/");

    let v_lower = run(
        r#"
import encoding
fn main() {
  match encoding.url_decode("%2f") {
    Ok(s) -> s
    Err(e) -> e
  }
}
"#,
    );
    assert_eq!(expect_string(v_lower), "/");
}

/// `+` in the input is a LITERAL `+` on output. Form-decoding is a
/// distinct concern; this primitive never does it.
#[test]
fn test_url_decode_plus_stays_literal() {
    let v = run(
        r#"
import encoding
fn main() {
  match encoding.url_decode("a+b") {
    Ok(s) -> s
    Err(e) -> e
  }
}
"#,
    );
    assert_eq!(expect_string(v), "a+b");
}

/// Truncated `%` at end of string must error.
#[test]
fn test_url_decode_truncated_percent_errors() {
    let v = run(
        r#"
import encoding
fn main() {
  match encoding.url_decode("bad%") {
    Ok(_) -> "wrong: should error"
    Err(e) -> e
  }
}
"#,
    );
    let s = expect_string(v);
    assert!(
        s.to_ascii_lowercase().contains("percent") || s.to_ascii_lowercase().contains("truncated"),
        "error should mention percent/truncated, got: {s}"
    );
}

/// `%` followed by non-hex digits must error.
#[test]
fn test_url_decode_non_hex_digits_errors() {
    let v = run(
        r#"
import encoding
fn main() {
  match encoding.url_decode("bad%ZZ") {
    Ok(_) -> "wrong: should error"
    Err(e) -> e
  }
}
"#,
    );
    let s = expect_string(v);
    assert!(
        s.to_ascii_lowercase().contains("invalid") || s.to_ascii_lowercase().contains("percent"),
        "error should mention invalid/percent, got: {s}"
    );
}

/// `%C3%28` decodes to bytes `0xC3 0x28` which is invalid UTF-8 (0xC3
/// starts a 2-byte sequence but 0x28 is not a continuation byte). The
/// percent sequences are each well-formed individually, so we don't
/// fail at the validation step — we fail at the UTF-8 step after the
/// bytes have been assembled.
#[test]
fn test_url_decode_invalid_utf8_errors() {
    let v = run(
        r#"
import encoding
fn main() {
  match encoding.url_decode("%C3%28") {
    Ok(_) -> "wrong: should error"
    Err(e) -> e
  }
}
"#,
    );
    let s = expect_string(v);
    assert!(
        s.to_ascii_lowercase().contains("utf-8") || s.to_ascii_lowercase().contains("utf8"),
        "error should mention UTF-8, got: {s}"
    );
}

// ── Round-trip ────────────────────────────────────────────────────────

/// `url_decode(url_encode(s)) == Ok(s)` for a set of tricky strings
/// covering emoji, quote characters, control characters, and the NUL
/// byte (which is legal inside a silt `String` because silt strings
/// are not C strings).
#[test]
fn test_round_trip_tricky_strings() {
    // Each input is embedded inside a silt `String` literal, so we need
    // to run a single silt program that concatenates the decoded output
    // back into a single comparable value. We emit a small test
    // harness per input that returns the decoded string; the Rust side
    // asserts equality.
    // Each case is written as it will appear verbatim inside a silt
    // `"..."` literal. Silt only supports a small escape set
    // (`\n \t \\ \" \{ \}`), so we avoid `\'` and other sequences the
    // lexer would reject.
    let cases: &[&str] = &[
        "hello world",
        "a & b = c ? d # e",
        "café ☕ 🚀",                      // multi-byte UTF-8 + emoji
        "quotes: \\\" and `backtick`",       // embeds \" — reads back as "
        "",                                  // empty string
        "just.a-simple_url.com/path",        // already safe; tests identity round-trip
    ];

    for input_silt in cases {
        let program = format!(
            r#"
import encoding
fn main() {{
  let s = "{input_silt}"
  match encoding.url_decode(encoding.url_encode(s)) {{
    Ok(back) -> back
    Err(e) -> e
  }}
}}
"#
        );
        let out = expect_string(run(&program));
        // Reconstruct the expected Rust-side string: unescape the
        // silt-level backslash sequences we embedded above.
        // Unescape the silt-level backslash sequences we embedded above
        // to get the Rust-side expected value. The order matters: handle
        // `\"` before `\\` so we don't double-process the backslash.
        let expected = input_silt.replace("\\\"", "\"").replace("\\\\", "\\");
        assert_eq!(
            out, expected,
            "round-trip failed for silt literal `{input_silt}`"
        );
    }
}

/// Separate test for the NUL byte. Silt string literals don't support
/// `\0`, so we build the NUL through `string.from_char_code(0)` and
/// concatenate it with ordinary text. The encoder must percent-escape
/// it (it's a control character, `%00`) and the decoder must restore
/// byte-identity round-trip.
#[test]
fn test_round_trip_nul_byte() {
    let v = run(
        r#"
import encoding
import string
fn main() {
  let nul = string.from_char_code(0)
  let s = "a" + nul + "b"
  let encoded = encoding.url_encode(s)
  match encoding.url_decode(encoded) {
    Ok(back) -> string.byte_length(back)
    Err(_) -> -1
  }
}
"#,
    );
    // "a" + NUL + "b" is 3 bytes. A correct round-trip preserves length.
    assert_eq!(v, Value::Int(3), "NUL byte did not round-trip");
}

/// Also pin the `%00` escape form — the encoder must percent-escape
/// the NUL byte (it's a control character, outside the unreserved set).
#[test]
fn test_url_encode_nul_byte_is_percent_zero_zero() {
    let v = run(
        r#"
import encoding
import string
fn main() {
  encoding.url_encode(string.from_char_code(0))
}
"#,
    );
    assert_eq!(expect_string(v), "%00");
}

/// Separate test for a newline — similar rationale to the NUL test.
#[test]
fn test_round_trip_newline() {
    let v = run(
        r#"
import encoding
fn main() {
  let s = "line1\nline2"
  match encoding.url_decode(encoding.url_encode(s)) {
    Ok(back) -> back
    Err(e) -> e
  }
}
"#,
    );
    assert_eq!(expect_string(v), "line1\nline2");
}

// ── Typechecker integration ───────────────────────────────────────────

#[test]
fn test_typechecker_accepts_encoding_signatures() {
    let errs = type_errors(
        r#"
import encoding
fn main() {
  let e = encoding.url_encode("hello world")
  let _ = encoding.url_decode(e)
}
"#,
    );
    assert!(errs.is_empty(), "got type errors: {errs:?}");
}

/// `encoding.url_encode` takes a `String`, not an `Int`. Passing an
/// integer literal must be rejected by the typechecker.
#[test]
fn test_typechecker_rejects_int_where_string_required() {
    let errs = type_errors(
        r#"
import encoding
fn main() {
  encoding.url_encode(42)
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected at least one type error, got none"
    );
}

// ── Docs / registration cross-check ───────────────────────────────────

/// Mirror of `test_documented_crypto_functions_match_registration`:
/// every function registered for the `encoding` module in
/// `src/module.rs` must appear in the per-module doc page.
#[test]
fn test_documented_encoding_functions_match_registration() {
    let doc_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("stdlib")
        .join("encoding.md");
    let body =
        std::fs::read_to_string(&doc_path).expect("failed to read docs/stdlib/encoding.md");

    let expected = silt::module::builtin_module_functions("encoding");
    assert!(
        !expected.is_empty(),
        "module::builtin_module_functions(\"encoding\") returned empty — registration is missing"
    );

    for name in &expected {
        let bare = format!("`{}`", name);
        let qualified = format!("encoding.{}", name);
        assert!(
            body.contains(&bare) || body.contains(&qualified),
            "docs/stdlib/encoding.md does not document the function `{name}`. \
             Every function in src/module.rs::builtin_module_functions(\"encoding\") \
             must be mentioned by name in the per-module doc."
        );
    }
}

// ── form_encode / form_decode ─────────────────────────────────────────

/// An empty pair list must produce an empty string (not `"="` or `"&"`).
/// We seed the list with a single pair and then use `list.tail` to get
/// an empty list of the right type (bare `[]` needs a type annotation
/// that's awkward to thread through the parser in this position).
#[test]
fn test_form_encode_empty_list_is_empty_string() {
    let v = run(
        r#"
import encoding
import list
fn main() {
  let seeded = [("k", "v")]
  let pairs = list.tail(seeded)
  encoding.form_encode(pairs)
}
"#,
    );
    assert_eq!(expect_string(v), "");
}

/// Ordering matters for form-encode: the input list order must be the
/// output order. This is the whole reason the signature is
/// `List((String, String))` rather than `Map(String, String)`.
#[test]
fn test_form_encode_preserves_order() {
    let v = run(
        r#"
import encoding
fn main() {
  encoding.form_encode([("a", "1"), ("b", "2"), ("c", "3")])
}
"#,
    );
    assert_eq!(expect_string(v), "a=1&b=2&c=3");
}

/// Space is `+` (form convention), `&` and `=` in values are `%26`/`%3D`.
#[test]
fn test_form_encode_space_plus_and_reserved() {
    let v = run(
        r#"
import encoding
fn main() {
  encoding.form_encode([("name", "Ada Lovelace"), ("role", "a & b = c")])
}
"#,
    );
    let s = expect_string(v);
    assert_eq!(s, "name=Ada+Lovelace&role=a+%26+b+%3D+c");
}

/// Non-ASCII UTF-8 bytes must be percent-escaped.
#[test]
fn test_form_encode_non_ascii() {
    let v = run(
        r#"
import encoding
fn main() {
  encoding.form_encode([("q", "café")])
}
"#,
    );
    assert_eq!(expect_string(v), "q=caf%C3%A9");
}

/// A literal `+` in input must be escaped as `%2B` so it does not
/// collide with the space convention on round-trip.
#[test]
fn test_form_encode_literal_plus_becomes_percent_2b() {
    let v = run(
        r#"
import encoding
fn main() {
  encoding.form_encode([("math", "1+1")])
}
"#,
    );
    assert_eq!(expect_string(v), "math=1%2B1");
}

/// form_decode: basic split and `+` → space.
#[test]
fn test_form_decode_basic_roundtrip() {
    let v = run(
        r#"
import encoding
fn main() {
  match encoding.form_decode("a=1&b=hello+world") {
    Ok(pairs) -> match pairs {
      [(_, v1), (_, v2)] -> v1 + "|" + v2
      _ -> "wrong-shape"
    }
    Err(e) -> e
  }
}
"#,
    );
    assert_eq!(expect_string(v), "1|hello world");
}

/// A segment with no `=` decodes to (key, "").
#[test]
fn test_form_decode_missing_equals_is_empty_value() {
    let v = run(
        r#"
import encoding
fn main() {
  match encoding.form_decode("flag") {
    Ok(pairs) -> match pairs {
      [(k, v)] -> k + "=" + v
      _ -> "wrong-shape"
    }
    Err(_) -> "err"
  }
}
"#,
    );
    assert_eq!(expect_string(v), "flag=");
}

/// Empty segments (leading `&`, `&&`, trailing `&`) are skipped.
#[test]
fn test_form_decode_empty_segments_skipped() {
    let v = run(
        r#"
import encoding
import list
fn main() {
  match encoding.form_decode("&a=1&&b=2&") {
    Ok(pairs) -> list.length(pairs)
    Err(_) -> -1
  }
}
"#,
    );
    assert_eq!(v, Value::Int(2));
}

/// Malformed percent escape in value must return Err, not silently pass.
#[test]
fn test_form_decode_bad_percent_errors() {
    let v = run(
        r#"
import encoding
fn main() {
  match encoding.form_decode("a=bad%ZZ") {
    Ok(_) -> "wrong: should error"
    Err(e) -> e
  }
}
"#,
    );
    let s = expect_string(v);
    let lower = s.to_ascii_lowercase();
    assert!(
        lower.contains("percent") || lower.contains("invalid"),
        "error should mention percent/invalid, got: {s}"
    );
}

/// form_encode → form_decode must round-trip shape and values.
#[test]
fn test_form_round_trip() {
    let v = run(
        r#"
import encoding
fn main() {
  let original = [("name", "Ada Lovelace"), ("q", "a & b = c"), ("lit", "1+1")]
  let body = encoding.form_encode(original)
  match encoding.form_decode(body) {
    Ok(pairs) -> match pairs {
      [(_, v0), (_, v1), (_, v2)] -> v0 + "|" + v1 + "|" + v2
      _ -> "wrong-shape"
    }
    Err(e) -> e
  }
}
"#,
    );
    assert_eq!(expect_string(v), "Ada Lovelace|a & b = c|1+1");
}

/// Every function registered for the encoding module must also have a
/// type signature in the type environment.
#[test]
fn test_every_encoding_function_has_a_type_signature() {
    let expected = silt::module::builtin_module_functions("encoding");
    for name in &expected {
        let input = format!(
            r#"
import encoding
fn main() {{
  let _ = encoding.{name}
}}
"#
        );
        let errs = type_errors(&input);
        for e in &errs {
            let lower = e.to_ascii_lowercase();
            assert!(
                !(lower.contains("unknown") && lower.contains(name.as_ref() as &str)),
                "encoding.{name} appears to be unregistered in the typechecker: {e}"
            );
        }
    }
}
