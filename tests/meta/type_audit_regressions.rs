//! Type-system audit regression kept in Rust: the Float `else`
//! narrowing operator is removed in stage 4, so this case is not a
//! golden. The rest of this file's cases are golden cases under
//! `tests/golden/meta/typecheck/type_audit_regressions__*.silt`.

use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

#[test]
fn test_float_else_does_not_trigger_spurious_unresolved_type() {
    // `x` is only referenced inside a float-else expression.
    // Before the fix, `expr_references_name` missed the FloatElse variant.
    let errs = type_errors(
        r#"
fn main() {
  let x = 1.0
  let y = x / 0.0 else 0.0
  println(y)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "float-else should not cause spurious type error, got: {errs:?}"
    );
}
