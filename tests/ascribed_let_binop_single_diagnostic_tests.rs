//! Round 60 G2 regression lock.
//!
//! The `ExprKind::Binary` arm of `infer_expr` calls `self.unify(&lt, &rt, span)`
//! (which can emit a "type mismatch" diagnostic at the RHS span) and then
//! returns `lt` (the still-non-Error lhs type). When this expression is
//! embedded in an ascribed let (`let n: Int = s + 1` with `s: String`),
//! the outer let-ascription unifies the returned Int with the binary op's
//! result — but because the binary op returned `String` (not `Type::Error`),
//! the cascade-suppression at `mod.rs:741` doesn't fire, and unify re-emits
//! the identical diagnostic at the same span.
//!
//! The fix snapshots `errors.len()` before the binary-op unify; if the
//! count grew, the Add arm returns `Type::Error` so the ascription's
//! outer unify hits the cascade-suppression branch and stays silent.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Core repro from the audit finding: the mismatch message must appear
/// exactly once, not twice.
///
/// Round 100 direction fix: the binop mismatch now reads left-to-right —
/// the first-seen (left) operand `s: String` establishes the expectation
/// and the right operand `1` is the "got" side, so the single inner
/// diagnostic is "expected String, got Int" (previously the inverted
/// "expected Int, got String"). The single-print invariant is unchanged.
#[test]
fn test_ascribed_let_binop_mismatch_prints_once() {
    let errs = type_errors(
        r#"
fn main() {
    let s: String = "hello"
    let n: Int = s + 1
    println(n)
}
"#,
    );
    let mismatch_count = errs.iter().filter(|e| e.contains("type mismatch")).count();
    assert_eq!(
        mismatch_count, 1,
        "expected exactly one type-mismatch diagnostic; got {mismatch_count} across:\n{errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("type mismatch: expected String, got Int")),
        "expected the round-100 left-establishes-expectation direction \
         ('expected String, got Int'); got:\n{errs:?}"
    );
}

/// Counterpart: when the types match, no diagnostic is emitted.
#[test]
fn test_ascribed_let_binop_matching_types_silent() {
    let errs = type_errors(
        r#"
fn main() {
    let a: Int = 1
    let n: Int = a + 2
    println(n)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "matching-type binop in ascribed let should be silent, got:\n{}",
        errs.join("\n")
    );
}
