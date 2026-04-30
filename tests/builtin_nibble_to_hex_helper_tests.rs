//! Lock test for round-64 DUP-3 fix.
//!
//! Round 64 collapsed two near-duplicate nibble→hex helpers — `bytes::hex_char`
//! (lowercase) and `encoding::upper_hex` (uppercase) — into a single shared
//! `super::common::nibble_to_hex(n, uppercase)`.
//!
//! Per audit rules, dead-code-removal fixes need a lock test proving the
//! deletion was a semantic no-op. The two function-output tests below are
//! the load-bearing locks: they exercise both call paths through the public
//! builtin API and assert byte-identical output to the old per-module
//! helpers' output tables.
//!
//! The shared helper itself lives in a private `mod common;` inside
//! `src/builtins/`, so it is only reachable from within the crate. The
//! integration-test path therefore drives it through the public APIs
//! (`bytes.to_hex` and `encoding.form_encode`), which is the same surface
//! every real caller uses.

use std::sync::Arc;

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

fn expect_string(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => panic!("expected String, got {other:?}"),
    }
}

/// Lowercase-table lock. `bytes.to_hex` of a byte `0xNN` must produce the
/// concatenation of the two lowercase nibble characters. Walk every nibble
/// position by feeding `bytes.from_hex` a known byte and round-tripping
/// it; the table here is the exact output the old `hex_char` helper would
/// have produced.
#[test]
fn nibble_to_hex_lowercase_matches_old_table() {
    let expected = [
        "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "a", "b", "c", "d", "e", "f",
    ];
    for n in 0u8..16 {
        // Build a byte whose low nibble is `n` and whose high nibble is 0.
        // `bytes.to_hex` will emit "0" + expected[n].
        let hex_input = format!("0{}", expected[n as usize]);
        let src = format!(
            r#"
import bytes
fn main() {{
  match bytes.from_hex("{hex_input}") {{
    Ok(b) -> bytes.to_hex(b)
    Err(e) -> e.message()
  }}
}}
"#
        );
        let got = expect_string(run(&src));
        assert_eq!(
            got, hex_input,
            "lowercase nibble {n}: bytes.to_hex must round-trip"
        );
        // The character in column 1 is the nibble-to-hex output for `n`.
        let nibble_char = &got[1..2];
        assert_eq!(
            nibble_char, expected[n as usize],
            "lowercase nibble {n} must render as {:?}, got {nibble_char:?}",
            expected[n as usize]
        );
    }
}

/// Uppercase-table lock. `encoding.form_encode` percent-escapes any non-
/// unreserved byte using upper-case hex. Pick bytes whose low nibble is
/// `n` and whose high nibble is 0 (the NUL..0x0F control range, all of
/// which need percent-escaping), and confirm the second hex digit is the
/// uppercase character the old `upper_hex` helper would have emitted.
#[test]
fn nibble_to_hex_uppercase_matches_old_table() {
    let expected = [
        "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "A", "B", "C", "D", "E", "F",
    ];
    for n in 0u8..16 {
        // Bytes 0x00..=0x0F are all controls, which form_escape_component
        // percent-encodes. To exercise the path through the public API we
        // build the corresponding key string and check the encoded output.
        // We use a static literal for each, since string literals can't
        // contain raw control bytes uniformly. Use bytes builtin to build
        // the key dynamically: `bytes.from_hex("0N")` then to_string.
        // (`encoding.form_encode` takes pairs of strings; controls inside
        // a UTF-8 string are still legal.) We pass the byte as the value
        // of a 1-element pair list.
        let hex_byte = format!("0{}", expected[n as usize].to_lowercase());
        let src = format!(
            r#"
import bytes
import encoding
fn main() {{
  match bytes.from_hex("{hex_byte}") {{
    Ok(b) -> match bytes.to_string(b) {{
      Ok(s) -> encoding.form_encode([("k", s)])
      Err(e) -> e.message()
    }}
    Err(e) -> e.message()
  }}
}}
"#
        );
        let got = expect_string(run(&src));
        // form_encode emits `k=%HH` where HH is the percent-escaped
        // representation of the single-byte value. The second hex digit
        // (index 4) is the nibble-to-hex output for `n` in upper case.
        // For n == 0 the expected output is "k=%00".
        let want = format!("k=%0{}", expected[n as usize]);
        assert_eq!(
            got, want,
            "uppercase nibble {n} must produce {want:?}, got {got:?}"
        );
    }
}

/// `bytes.to_hex` must use lowercase a–f. The old `hex_char` helper
/// hard-coded `b'a' + (n - 10)`; this lock pins that contract through the
/// public API after the helper deletion.
#[test]
fn bytes_to_hex_emits_lowercase() {
    let v = run(r#"
import bytes
fn main() {
  match bytes.from_hex("deadbeefABCDEF") {
    Ok(b) -> bytes.to_hex(b)
    Err(e) -> e.message()
  }
}
"#);
    let s = expect_string(v);
    assert_eq!(
        s, "deadbeefabcdef",
        "bytes.to_hex must emit lowercase hex digits"
    );
    assert!(
        !s.chars().any(|c| c.is_ascii_uppercase()),
        "bytes.to_hex must never emit uppercase hex: {s}"
    );
}

/// `encoding.form_encode` (and `encoding.url_encode`) must use uppercase
/// A–F for percent-escaped bytes. The old `upper_hex` helper hard-coded
/// `b'A' + (nibble - 10)`; this lock pins that contract through the
/// public API after the helper deletion.
#[test]
fn encoding_hex_emits_uppercase() {
    // Use literal UTF-8 chars whose multi-byte encoding contains the full
    // a–f hex range. «»íï give us 0xC2 0xAB, 0xC2 0xBB, 0xC3 0xAD, 0xC3 0xAF.
    let v = run(
        "
import encoding
fn main() {
  encoding.form_encode([(\"k\", \"\u{00ab}\u{00bb}\u{00ed}\u{00ef}\")])
}
",
    );
    let s = expect_string(v);
    // We expect every hex digit to be uppercase.
    assert!(
        !s.chars().any(|c| ('a'..='f').contains(&c)),
        "encoding.form_encode must never emit lowercase hex: {s}"
    );
    // Spot-check the exact escape so a future regression that swaps casing
    // (or changes the digit table) is caught.
    assert_eq!(
        s, "k=%C2%AB%C2%BB%C3%AD%C3%AF",
        "encoding.form_encode percent-escape format must remain stable"
    );
}
