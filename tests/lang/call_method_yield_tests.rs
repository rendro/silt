//! `Op::ListConcat` combined-size pre-check.
//!
//! `Op::ListConcat` validated each operand individually (max 10M) but only
//! checked the combined size after materializing both. Two 9.9M ranges would
//! cause ~800MB allocation before being rejected. The fix adds a pre-check on
//! `result.len() + b_len` before extending.
//!
//! Spreading a Range is rejected by the typechecker, so this runtime defence
//! is only reachable by running an ill-typed program in process; the other
//! tests of this file are golden cases (`tests/golden/lang/*/call_method_yield__*`).

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;
use std::sync::Arc;

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

// ── ListConcat combined-size pre-check ────────────────────────────────

/// Two ranges whose individual sizes are under the 10M cap but whose
/// combined size exceeds it must be rejected WITHOUT materializing both.
/// Before the fix, this would allocate ~800MB before failing.
/// Op::ListConcat is emitted for list-spread syntax `[..a, ..b]`.
#[test]
fn test_list_concat_combined_size_rejected_before_materialize() {
    let err = run_err(
        r#"
fn main() {
  let a = 1..6_000_000
  let b = 1..6_000_000
  [..a, ..b]
}
"#,
    );
    assert!(
        err.contains("concatenated list exceeds maximum size"),
        "expected combined-size rejection, got: {err}"
    );
}
