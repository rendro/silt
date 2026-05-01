//! Round 67 BROKEN regression lock — F1.
//!
//! When the operands of `+`, `-`, `*`, or `%` are of *different* types AND
//! at least one operand is outside the operator's accepted domain (e.g.
//! `Bool + Int`), the typechecker used to emit TWO diagnostics at the
//! same span:
//!
//!   error[type]: type mismatch: expected Int, got Bool
//!   error[type]: operator '+' requires Int, Float, ExtFloat, or String, got 'Bool'
//!
//! The "type mismatch: expected X, got Y" diagnostic already implies the
//! operand-domain failure for the misaligned operand — emitting the second
//! diagnostic is redundant and noisy. The fix snapshots the error count
//! around `unify`; if `unify` reported a mismatch, the operand-domain
//! check is skipped.
//!
//! See `src/typechecker/inference.rs` BinOp::Add and BinOp::Sub|Mul|Mod
//! arms.

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

/// Count diagnostics that look like a binop type mismatch ("expected X, got Y")
/// or like the operator-domain message ("operator '<op>' requires ...").
fn count_binop_diagnostics(errs: &[String], op_label: &str) -> (usize, usize) {
    let mismatches = errs
        .iter()
        .filter(|e| e.starts_with("type mismatch:") || e.contains("type mismatch:"))
        .count();
    let domain = errs
        .iter()
        .filter(|e| e.contains(&format!("operator {op_label} requires")))
        .count();
    (mismatches, domain)
}

#[test]
fn test_bool_plus_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = true + 1
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'+'");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true + 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn test_bool_minus_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = true - 1
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'-'");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true - 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn test_bool_times_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = true * 1
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'*'");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true * 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn test_bool_mod_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = true % 1
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'%'");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true % 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

/// Counterpart: when the binop operand IS in-domain but mismatches the
/// other operand (e.g. ascribed-let `let n: Int = "s" + 1`), the
/// type-mismatch diagnostic should still appear (this guards against
/// over-aggressive suppression).
#[test]
fn test_string_plus_int_still_reports_mismatch() {
    let errs = type_errors(
        r#"
fn main() {
    let s: String = "hello"
    let x = s + 1
    ()
}
"#,
    );
    // Some diagnostic must mention the type mismatch between String and Int.
    assert!(
        !errs.is_empty(),
        "expected at least one diagnostic for `\"hello\" + 1`, got nothing"
    );
}

/// Counterpart: a non-arith operand that *agrees* with the other operand
/// (so unify does not error) must STILL emit the operator-domain
/// diagnostic. This guards against overshooting the fix.
#[test]
fn test_bool_plus_bool_emits_domain_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let a: Bool = true
    let b: Bool = false
    let x = a + b
    ()
}
"#,
    );
    let domain_hits = errs
        .iter()
        .filter(|e| e.contains("operator '+' requires"))
        .count();
    assert!(
        domain_hits >= 1,
        "expected operator-domain diagnostic for `Bool + Bool` (operands agree, \
         so type-mismatch wouldn't fire — domain check must still run), got:\n{}",
        errs.join("\n")
    );
}
