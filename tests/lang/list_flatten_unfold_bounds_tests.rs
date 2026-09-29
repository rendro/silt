//! Regression tests for `list.flatten` and `list.unfold` accumulation caps.
//!
//! Round 19 audit -- LATENT: both `list.flatten` and `list.unfold` could
//! accumulate unbounded results without ever checking `MAX_RANGE_MATERIALIZE`.
//! Every other collection-building builtin caps output at 10,000,000 elements.
//! The `list.unfold` cap test stays in Rust because driving ten million
//! closure calls through the CLI exceeds the golden harness's 20 s case
//! timeout; the `list.flatten` cap and both small-input controls are golden
//! cases `tests/golden/lang/limits/list_flatten_unfold_bounds__*`.

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

// ── list.unfold ────────────────────────────────────────────────────────

/// Unfold that would generate more than MAX_RANGE_MATERIALIZE elements.
/// The callback never returns None, so without a cap it would loop forever.
/// With the cap it should error after 10,000,001 elements.
#[test]
fn test_list_unfold_over_cap_rejected() {
    let err = run_err(
        r#"
import list
fn main() {
  list.unfold(0) { n -> Some((n, n + 1)) }
}
        "#,
    );
    assert!(
        err.contains("list.unfold"),
        "error should mention list.unfold by name, got: {err}"
    );
    assert!(
        err.contains("exceeds maximum list length"),
        "error should mention exceeds maximum list length, got: {err}"
    );
}
