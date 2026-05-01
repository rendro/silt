//! Round 67 sibling-miss + message-unification lock — the four arms of
//! `Vm::compare` that surface a partial_cmp failure on non-finite
//! floats must produce ONE error wording, not two.
//!
//! Pre-fix `src/vm/arithmetic.rs:118-132`:
//!   - `(Float, Float)` → "cannot compare non-finite float values"
//!   - `(ExtFloat, ExtFloat)` → "cannot compare NaN values"
//!   - `(Float, ExtFloat)` → "cannot compare NaN values"
//!   - `(ExtFloat, Float)` → "cannot compare NaN values"
//! Two issues: byte-identical NaN boilerplate copied 3× and divergent
//! error wording for semantically identical conditions. Round 67
//! collapses to one wording.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;
use std::sync::Arc;

const ARITHMETIC_RS: &str = include_str!("../src/vm/arithmetic.rs");

/// Unified wording that the round-67 fix lands on. Documents the
/// canonical form so future parallel surfaces can adopt it.
const UNIFIED_WORDING: &str = "cannot compare non-finite float values";

// ── Source-grep locks ───────────────────────────────────────────────

#[test]
fn arithmetic_rs_uses_unified_compare_error_wording() {
    assert!(
        ARITHMETIC_RS.contains(UNIFIED_WORDING),
        "src/vm/arithmetic.rs no longer contains the unified wording \
         `{UNIFIED_WORDING}` — round 67 unified all four NaN/non-finite \
         compare arms to this exact form."
    );
}

#[test]
fn arithmetic_rs_does_not_use_old_nan_wording() {
    // Match the bug form as a string literal (opening quote + body) so
    // doc-comments / changelog notes that *describe* the historical
    // wording in prose don't trip the lock. This mirrors the
    // `"\"internal:"` lock pattern from
    // `tests/builtin_error_message_lock_tests.rs`.
    assert!(
        !ARITHMETIC_RS.contains("\"cannot compare NaN values\""),
        "src/vm/arithmetic.rs still contains the old `\"cannot compare \
         NaN values\"` string literal. Round 67 unified all NaN/non-finite \
         compare arms in this file to `{UNIFIED_WORDING}`."
    );
}

// ── Behavioral lock ─────────────────────────────────────────────────

fn run_to_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parser");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler
        .compile_program(&program)
        .expect("compile");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).err().expect("expected VmError");
    err.message
}

#[test]
fn compare_extfloat_nan_uses_unified_wording_at_runtime() {
    // `Float / Float` widens to `ExtFloat` per the round-58 widening
    // rules, and `0.0 / 0.0` is NaN. Comparing two ExtFloat NaNs with
    // `<` walks the `(ExtFloat, ExtFloat)` arm in `Vm::compare` —
    // pre-round-67 this surfaced "cannot compare NaN values".
    let src = r#"
fn main() {
  let a: Float = 0.0
  let b: Float = 0.0
  let nan = a / b
  let other = a / b
  println(nan < other)
}
"#;
    let msg = run_to_err(src);
    assert!(
        msg.contains(UNIFIED_WORDING),
        "expected unified wording `{UNIFIED_WORDING}` in error, got: {msg}"
    );
    assert!(
        !msg.contains("cannot compare NaN values"),
        "old wording `cannot compare NaN values` still surfaced at \
         runtime: {msg}"
    );
}
