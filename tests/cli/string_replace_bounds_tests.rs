//! Regression tests for `string.replace` result-length bounds (L2).
//!
//! Before the fix, `string.replace` called Rust's `str::replace`
//! directly with no result-size cap. A call like
//! `s.replace("", long_to)` inserts `to` at every byte boundary,
//! producing `(|s| + 1) * |to| + |s|` bytes — trivially gigabytes
//! from modestly-sized inputs. The fix pre-computes the worst-case
//! result length and rejects calls that would exceed
//! `MAX_RANGE_MATERIALIZE`, matching the existing caps on
//! `string.repeat`, `string.pad_left`, and `string.pad_right`.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

// ── Empty `from` + long `to` rejected ────────────────────────────────
//
// `string.repeat("a", 100_000)` is 100k bytes, `string.repeat("x", 10_000)`
// is 10k bytes. With an empty `from`, Rust's `str::replace` inserts
// `to` at every byte boundary: `(100_000 + 1) * 10_000 + 100_000`
// = 1,000,110,000 bytes (~1 GiB). Must be rejected with an exact phrase
// pin.

#[test]
fn test_string_replace_empty_from_huge_to_rejected() {
    let err = run_err(
        r#"
import string
fn main() {
  let s = string.repeat("a", 100000)
  let t = string.repeat("x", 10000)
  string.replace(s, "", t)
}
        "#,
    );
    assert!(
        err.contains("string.replace: result would exceed maximum string size"),
        "error should mention string.replace cap, got: {err}"
    );
    assert!(
        err.contains("10000000 limit"),
        "error should mention the 10_000_000-byte limit, got: {err}"
    );
}

// ── Empty `from` + short `to` uses Rust semantics ────────────────────
//
// Rust's `str::replace("abc", "", "X")` returns `"XaXbXcX"` —
// insertion at every boundary including before the first and after
// the last char. The fix must preserve this exact behaviour for
// inputs below the cap.

#[test]
fn test_string_replace_empty_from_small_to_ok() {
    let result = run(r#"
import string
fn main() {
  string.replace("abc", "", "X")
}
        "#);
    assert_eq!(result, Value::String("XaXbXcX".to_string()));
}

// ── Normal case: well below cap, non-empty `from` ───────────────────
//
// Classic `replace` behaviour must be preserved.

#[test]
fn test_string_replace_normal_case_ok() {
    let result = run(r#"
import string
fn main() {
  string.replace("hello world", "world", "silt")
}
        "#);
    assert_eq!(result, Value::String("hello silt".to_string()));
}

// ── Exactly at the cap: must pass ───────────────────────────────────
//
// `MAX_RANGE_MATERIALIZE` is 10_000_000 bytes. Build a case whose
// worst-case result length (per the fix's formula) equals the cap
// exactly:
//
//   from = "ab" (len 2), to = "xy" (len 2), s = repeat("ab", 5_000_000)
//   s_len = 10_000_000
//   occurrences = 5_000_000
//   since to_len >= from_len: result_len = 10_000_000 + 5_000_000 * 0
//                             = 10_000_000  (exactly at the cap)
//
// The replace must succeed and yield a 10_000_000-char result.

#[test]
fn test_string_replace_at_cap_ok() {
    let result = run(r#"
import string
fn main() {
  let s = string.repeat("ab", 5000000)
  string.length(string.replace(s, "ab", "xy"))
}
        "#);
    assert_eq!(result, Value::Int(10_000_000));
    // Suppress unused-import warning without needing a real list.
    let _ = Arc::new(vec![Value::Int(0)]);
}
