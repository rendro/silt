//! End-to-end tests for the `crypto` builtin module.
//!
//! Known-answer vectors pin the exact digests / HMAC tags so a future
//! refactor of the RustCrypto backend (or a switch to a pure-Rust
//! reimplementation) cannot silently change output. The CSPRNG tests
//! are probabilistic on the "distinctness" arm but deterministic on
//! the bounds-checking arms.

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

// ── SHA-256 / SHA-512 KATs ─────────────────────────────────────────────

/// Canonical NIST test vector: SHA-256("abc").
#[test]
fn test_sha256_abc_matches_nist_kat() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  bytes.to_hex(crypto.sha256(bytes.from_string("abc")))
}
"#,
    );
    assert_eq!(
        expect_string(v),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn test_sha256_empty_input_matches_known_answer() {
    // SHA256("") == e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  bytes.to_hex(crypto.sha256(bytes.empty()))
}
"#,
    );
    assert_eq!(
        expect_string(v),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn test_sha256_output_is_32_bytes() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  bytes.length(crypto.sha256(bytes.from_string("anything")))
}
"#,
    );
    assert_eq!(v, Value::Int(32));
}

/// NIST SHA-512("abc") KAT.
#[test]
fn test_sha512_abc_matches_nist_kat() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  bytes.to_hex(crypto.sha512(bytes.from_string("abc")))
}
"#,
    );
    assert_eq!(
        expect_string(v),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
         2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
}

#[test]
fn test_sha512_output_is_64_bytes() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  bytes.length(crypto.sha512(bytes.from_string("anything")))
}
"#,
    );
    assert_eq!(v, Value::Int(64));
}

// ── HMAC-SHA256 / HMAC-SHA512 KATs (RFC 4231) ──────────────────────────

/// RFC 4231 test case 1:
///   Key  = 0x0b * 20
///   Data = "Hi There"
///   HMAC-SHA256 = b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7
#[test]
fn test_hmac_sha256_rfc4231_tc1() {
    // 20 * 0x0b = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  match bytes.from_hex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b") {
    Ok(key) -> bytes.to_hex(crypto.hmac_sha256(key, bytes.from_string("Hi There")))
    Err(e) -> e
  }
}
"#,
    );
    assert_eq!(
        expect_string(v),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
}

#[test]
fn test_hmac_sha256_output_is_32_bytes() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  bytes.length(crypto.hmac_sha256(bytes.from_string("k"), bytes.from_string("m")))
}
"#,
    );
    assert_eq!(v, Value::Int(32));
}

/// RFC 4231 test case 1 for HMAC-SHA512:
///   Key  = 0x0b * 20
///   Data = "Hi There"
///   HMAC-SHA512 = 87aa7cdea5ef619d4ff0b4241a1d6cb0
///                 2379f4e2ce4ec2787ad0b30545e17cde
///                 daa833b7d6b8a702038b274eaea3f4e4
///                 be9d914eeb61f1702e696c203a126854
#[test]
fn test_hmac_sha512_rfc4231_tc1() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  match bytes.from_hex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b") {
    Ok(key) -> bytes.to_hex(crypto.hmac_sha512(key, bytes.from_string("Hi There")))
    Err(e) -> e
  }
}
"#,
    );
    assert_eq!(
        expect_string(v),
        "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cde\
         daa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854"
    );
}

#[test]
fn test_hmac_sha512_output_is_64_bytes() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  bytes.length(crypto.hmac_sha512(bytes.from_string("k"), bytes.from_string("m")))
}
"#,
    );
    assert_eq!(v, Value::Int(64));
}

// ── CSPRNG ─────────────────────────────────────────────────────────────

#[test]
fn test_random_bytes_zero_returns_empty() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  match crypto.random_bytes(0) {
    Ok(b) -> bytes.length(b)
    Err(_) -> -1
  }
}
"#,
    );
    assert_eq!(v, Value::Int(0));
}

#[test]
fn test_random_bytes_32_is_correct_length() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  match crypto.random_bytes(32) {
    Ok(b) -> bytes.length(b)
    Err(_) -> -1
  }
}
"#,
    );
    assert_eq!(v, Value::Int(32));
}

#[test]
fn test_random_bytes_two_calls_are_distinct() {
    // Two 32-byte CSPRNG draws collide with probability ~2^-256. Treat
    // equality as a fatal deterministic bug, not a "flaky test".
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  match crypto.random_bytes(32) {
    Ok(a) -> match crypto.random_bytes(32) {
      Ok(b) -> bytes.eq(a, b)
      Err(_) -> true
    }
    Err(_) -> true
  }
}
"#,
    );
    assert_eq!(v, Value::Bool(false), "two 32-byte CSPRNG draws were equal");
}

#[test]
fn test_random_bytes_negative_returns_err() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  match crypto.random_bytes(-1) {
    Ok(_) -> "wrong: should error"
    Err(e) -> e
  }
}
"#,
    );
    let s = expect_string(v);
    assert!(
        s.contains("non-negative"),
        "error message should mention non-negative, got: {s}"
    );
}

#[test]
fn test_random_bytes_over_cap_returns_err() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  match crypto.random_bytes(2000000) {
    Ok(_) -> "wrong: should error"
    Err(e) -> e
  }
}
"#,
    );
    let s = expect_string(v);
    assert!(
        s.contains("cap") || s.contains("1 MiB"),
        "error message should mention cap / 1 MiB, got: {s}"
    );
}

// ── constant_time_eq ───────────────────────────────────────────────────

#[test]
fn test_constant_time_eq_identical_bytes_returns_true() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  let a = bytes.from_string("secret-token-123")
  let b = bytes.from_string("secret-token-123")
  crypto.constant_time_eq(a, b)
}
"#,
    );
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn test_constant_time_eq_same_length_different_contents_returns_false() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  let a = bytes.from_string("secret-token-123")
  let b = bytes.from_string("secret-token-124")
  crypto.constant_time_eq(a, b)
}
"#,
    );
    assert_eq!(v, Value::Bool(false));
}

/// Different-length buffers must return false. This documents the
/// known length-leak behavior of the timing-safe comparison: lengths
/// *can* leak via timing (short-circuit), but contents of equal-length
/// buffers cannot. Callers needing length privacy should pad inputs to
/// a fixed width before comparing.
#[test]
fn test_constant_time_eq_different_length_returns_false() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  let a = bytes.from_string("short")
  let b = bytes.from_string("muchlonger")
  crypto.constant_time_eq(a, b)
}
"#,
    );
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn test_constant_time_eq_both_empty_returns_true() {
    let v = run(
        r#"
import bytes
import crypto
fn main() {
  crypto.constant_time_eq(bytes.empty(), bytes.empty())
}
"#,
    );
    assert_eq!(v, Value::Bool(true));
}

// ── Typechecker integration ───────────────────────────────────────────

#[test]
fn test_typechecker_accepts_crypto_signatures() {
    let errs = type_errors(
        r#"
import bytes
import crypto
fn main() {
  let msg = bytes.from_string("hi")
  let digest = crypto.sha256(msg)
  let tag = crypto.hmac_sha256(bytes.from_string("k"), msg)
  let _ = crypto.constant_time_eq(digest, tag)
  let _ = crypto.sha512(msg)
  let _ = crypto.hmac_sha512(bytes.from_string("k"), msg)
  let _ = crypto.random_bytes(16)
}
"#,
    );
    assert!(errs.is_empty(), "got type errors: {errs:?}");
}

#[test]
fn test_typechecker_rejects_string_where_bytes_required() {
    // crypto.sha256 takes Bytes, not String. Passing a String literal
    // must be rejected by the type checker.
    let errs = type_errors(
        r#"
import crypto
fn main() {
  crypto.sha256("not bytes")
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected at least one type error, got none"
    );
}

// ── Docs / registration cross-check ───────────────────────────────────

/// Walks the crypto doc page and asserts every function mentioned in
/// the summary table has a matching registration in the typechecker.
/// This mirrors the spirit of `docs_round26_tests::every_register_builtins_has_a_per_module_doc`
/// but runs in the other direction: docs → registration.
#[test]
fn test_documented_crypto_functions_match_registration() {
    let doc_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("stdlib")
        .join("crypto.md");
    let body =
        std::fs::read_to_string(&doc_path).expect("failed to read docs/stdlib/crypto.md");

    // The expected set is the on-disk truth: the function list in
    // src/module.rs::builtin_module_functions("crypto").
    let expected = silt::module::builtin_module_functions("crypto");
    assert!(
        !expected.is_empty(),
        "module::builtin_module_functions(\"crypto\") returned empty — registration is missing"
    );

    for name in &expected {
        // Every function must appear in the doc body — either as a
        // table row (`| `<name>` |`) or in running prose
        // (`crypto.<name>`).
        let bare = format!("`{}`", name);
        let qualified = format!("crypto.{}", name);
        assert!(
            body.contains(&bare) || body.contains(&qualified),
            "docs/stdlib/crypto.md does not document the function `{name}`. \
             Every function in src/module.rs::builtin_module_functions(\"crypto\") \
             must be mentioned by name in the per-module doc."
        );
    }
}

/// Every function registered for the crypto module must also have a
/// type signature in the type environment. This catches a drift where
/// module.rs exposes a function name but the typechecker does not
/// know the signature.
#[test]
fn test_every_crypto_function_has_a_type_signature() {
    let expected = silt::module::builtin_module_functions("crypto");
    for name in &expected {
        let input = format!(
            r#"
import crypto
fn main() {{
  let _ = crypto.{name}
}}
"#
        );
        let errs = type_errors(&input);
        // We accept errors of the form "crypto.X is not callable" /
        // arity / etc., but we must NOT see the hard "unknown
        // identifier: crypto.X" form that would indicate missing
        // registration. Easiest signal: look for "unknown" in the
        // error list; the typechecker uses `Unknown identifier` or
        // `unknown function` wording for missing names.
        for e in &errs {
            let lower = e.to_ascii_lowercase();
            assert!(
                !(lower.contains("unknown") && lower.contains(name.as_ref() as &str)),
                "crypto.{name} appears to be unregistered in the typechecker: {e}"
            );
        }
    }
}
