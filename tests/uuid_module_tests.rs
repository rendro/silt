//! End-to-end tests for the `uuid` builtin module.
//!
//! Mirrors the shape of `tests/crypto_module_tests.rs`: each test drives
//! the VM via the same lex → parse → typecheck → compile → run
//! pipeline, then asserts on the returned `Value`. Tests cover the
//! full advertised API surface (`v4`, `v7`, `parse`, `nil`,
//! `is_valid`) plus a couple of typechecker / registration cross-checks
//! so a future drift between the runtime, typechecker, and the
//! `src/module.rs` function list will fail loudly instead of silently.

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

fn expect_tuple2(v: Value) -> (Value, Value) {
    match v {
        Value::Tuple(mut xs) if xs.len() == 2 => {
            let b = xs.pop().unwrap();
            let a = xs.pop().unwrap();
            (a, b)
        }
        other => panic!("expected 2-tuple, got {other:?}"),
    }
}

/// Canonical 8-4-4-4-12 form: 36 chars, hyphens at positions 8, 13, 18, 23,
/// every non-hyphen char is a lowercase hex digit. Returns `Ok(())` on
/// match, `Err(reason)` otherwise.
fn assert_canonical(s: &str) {
    assert_eq!(s.len(), 36, "expected 36-char UUID, got {} chars: {s:?}", s.len());
    let b = s.as_bytes();
    for (i, byte) in b.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => assert_eq!(
                *byte, b'-',
                "expected '-' at position {i} in {s:?}"
            ),
            _ => {
                let ok = byte.is_ascii_digit() || (b'a'..=b'f').contains(byte);
                assert!(
                    ok,
                    "expected lowercase hex at position {i} in {s:?}, got {:?}",
                    *byte as char
                );
            }
        }
    }
}

// ── v4 ──────────────────────────────────────────────────────────────────

/// `uuid.v4` returns a 36-char canonical lowercase hyphenated UUID, and
/// two consecutive calls return different strings (collision probability
/// ~2^-122, treated as impossible).
#[test]
fn test_v4_returns_canonical_form_and_is_unique_across_calls() {
    let v = run(
        r#"
import uuid
fn main() {
  (uuid.v4(), uuid.v4())
}
"#,
    );
    let (a, b) = expect_tuple2(v);
    let a = expect_string(a);
    let b = expect_string(b);
    assert_canonical(&a);
    assert_canonical(&b);
    assert_ne!(
        a, b,
        "two consecutive uuid.v4 calls produced the same value — \
         either the RNG is broken or the function is cached"
    );
}

// ── v7 ──────────────────────────────────────────────────────────────────

/// `uuid.v7` returns a 36-char canonical UUID, and two consecutive calls
/// sort in generation order via straight lexicographic string
/// comparison. That monotonic property is the whole point of v7.
#[test]
fn test_v7_returns_canonical_form_and_sorts_in_generation_order() {
    let v = run(
        r#"
import uuid
fn main() {
  (uuid.v7(), uuid.v7())
}
"#,
    );
    let (a, b) = expect_tuple2(v);
    let a = expect_string(a);
    let b = expect_string(b);
    assert_canonical(&a);
    assert_canonical(&b);
    // v7 is *monotonic non-decreasing* in wall time. Within a single
    // millisecond, the spec permits equal timestamps (the trailing
    // random bits then act as a tiebreak and are not ordered). So the
    // correct assertion is `a <= b`, NOT `a < b`: asserting strict
    // ordering would be a flake waiting to happen.
    assert!(
        a <= b,
        "consecutive uuid.v7 output should lex-sort non-decreasing; got a={a:?} > b={b:?}"
    );
}

// ── parse ───────────────────────────────────────────────────────────────

/// `uuid.parse` must:
/// - accept valid UUIDs regardless of input casing,
/// - canonicalize the output to lowercase hyphenated form,
/// - reject malformed input with an `Err(msg)`.
#[test]
fn test_parse_accepts_valid_canonicalizes_rejects_malformed() {
    // Uppercase input → lowercase canonical output.
    let v = run(
        r#"
import uuid
fn main() {
  match uuid.parse("550E8400-E29B-41D4-A716-446655440000") {
    Ok(s) -> s
    Err(e) -> e
  }
}
"#,
    );
    assert_eq!(
        expect_string(v),
        "550e8400-e29b-41d4-a716-446655440000",
        "uuid.parse must canonicalize uppercase input to lowercase"
    );

    // Already-canonical lowercase input → unchanged.
    let v = run(
        r#"
import uuid
fn main() {
  match uuid.parse("550e8400-e29b-41d4-a716-446655440000") {
    Ok(s) -> s
    Err(e) -> e
  }
}
"#,
    );
    assert_eq!(
        expect_string(v),
        "550e8400-e29b-41d4-a716-446655440000"
    );

    // Malformed input → Err branch taken, error message surfaces.
    let v = run(
        r#"
import uuid
fn main() {
  match uuid.parse("not-a-uuid") {
    Ok(_) -> "wrong: should have errored"
    Err(e) -> e
  }
}
"#,
    );
    let s = expect_string(v);
    assert!(
        s.to_ascii_lowercase().contains("invalid") || s.to_ascii_lowercase().contains("uuid"),
        "Err message should mention the failure, got: {s:?}"
    );

    // A second malformed form (wrong length): make sure the check
    // doesn't trip on only-one-shape-of-malformed.
    let v = run(
        r#"
import uuid
fn main() {
  match uuid.parse("550e8400e29b41d4a71644665544") {
    Ok(_) -> "wrong"
    Err(_) -> "err"
  }
}
"#,
    );
    assert_eq!(expect_string(v), "err");
}

// ── nil ─────────────────────────────────────────────────────────────────

/// `uuid.nil` returns exactly the all-zero UUID.
#[test]
fn test_nil_returns_all_zero_uuid() {
    let v = run(
        r#"
import uuid
fn main() {
  uuid.nil()
}
"#,
    );
    assert_eq!(
        expect_string(v),
        "00000000-0000-0000-0000-000000000000"
    );
}

// ── is_valid ────────────────────────────────────────────────────────────

/// `uuid.is_valid` must agree with `uuid.parse`'s Ok/Err classification
/// across well-formed and malformed inputs. This is the contract that
/// callers rely on when they skip the `Result` wrapper for a hot-path
/// boolean check.
#[test]
fn test_is_valid_matches_parse_ok_err_classification() {
    // Valid → true.
    let v = run(
        r#"
import uuid
fn main() {
  uuid.is_valid("550e8400-e29b-41d4-a716-446655440000")
}
"#,
    );
    assert_eq!(v, Value::Bool(true));

    // Mixed-case valid → true (parser is case-insensitive).
    let v = run(
        r#"
import uuid
fn main() {
  uuid.is_valid("550E8400-e29b-41D4-A716-446655440000")
}
"#,
    );
    assert_eq!(v, Value::Bool(true));

    // Nil UUID is a syntactically valid UUID.
    let v = run(
        r#"
import uuid
fn main() {
  uuid.is_valid("00000000-0000-0000-0000-000000000000")
}
"#,
    );
    assert_eq!(v, Value::Bool(true));

    // Malformed → false.
    let v = run(
        r#"
import uuid
fn main() {
  uuid.is_valid("not-a-uuid")
}
"#,
    );
    assert_eq!(v, Value::Bool(false));

    // Empty → false.
    let v = run(
        r#"
import uuid
fn main() {
  uuid.is_valid("")
}
"#,
    );
    assert_eq!(v, Value::Bool(false));

    // Freshly generated v4 / v7 both classify as valid.
    let v = run(
        r#"
import uuid
fn main() {
  (uuid.is_valid(uuid.v4()), uuid.is_valid(uuid.v7()))
}
"#,
    );
    let (a, b) = expect_tuple2(v);
    assert_eq!(a, Value::Bool(true));
    assert_eq!(b, Value::Bool(true));
}

// ── Typechecker + registration cross-checks ────────────────────────────

#[test]
fn test_typechecker_accepts_uuid_signatures() {
    let errs = type_errors(
        r#"
import uuid
fn main() {
  let _ = uuid.v4()
  let _ = uuid.v7()
  let _ = uuid.nil()
  let _ = uuid.is_valid("x")
  let _ = uuid.parse("x")
}
"#,
    );
    assert!(errs.is_empty(), "got type errors: {errs:?}");
}

/// Every function registered in `src/module.rs::builtin_module_functions("uuid")`
/// must have a type signature so that `uuid.<fn>` resolves in the
/// typechecker. Catches drift where a new function is added to
/// module.rs but the typechecker/runtime never learn about it.
#[test]
fn test_every_uuid_function_has_a_type_signature() {
    let expected = silt::module::builtin_module_functions("uuid");
    assert!(
        !expected.is_empty(),
        "module::builtin_module_functions(\"uuid\") returned empty"
    );
    for name in &expected {
        let input = format!(
            r#"
import uuid
fn main() {{
  let _ = uuid.{name}
}}
"#
        );
        let errs = type_errors(&input);
        for e in &errs {
            let lower = e.to_ascii_lowercase();
            assert!(
                !(lower.contains("unknown") && lower.contains(name.as_ref() as &str)),
                "uuid.{name} appears to be unregistered in the typechecker: {e}"
            );
        }
    }
}
