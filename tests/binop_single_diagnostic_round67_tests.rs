//! Round 67 BROKEN regression lock — F1, extended in round 72.
//!
//! When the operands of `+`, `-`, `*`, `%`, `/`, `<`, `>`, `<=`, `>=`,
//! `==`, or `!=` are of *different* types AND at least one operand is
//! outside the operator's accepted domain (e.g. `Bool + Int`), the
//! typechecker used to emit TWO diagnostics at the same span:
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
//! Round 67 fixed Add/Sub/Mul/Mod. Round 72 extended the fix to Div,
//! Lt/Gt/Leq/Geq (same dual-emit bug), and applied the snapshot pattern
//! defensively to Eq/Neq to close the latent door.
//!
//! Round 100: the mechanism moved into
//! `TypeChecker::unify_binop_operands` (which also fixes the
//! expected/got DIRECTION — see
//! `tests/binop_mismatch_direction_round100_tests.rs`). WHICH of the
//! two diagnostics is emitted changed for lone out-of-domain operands
//! (the operand-domain message now wins over a misdirected mismatch),
//! but the single-diagnostic invariant locked here is unchanged: these
//! tests count mismatch + domain and assert the sum is exactly 1.
//!
//! See `src/typechecker/inference.rs` BinOp::Add, BinOp::Sub|Mul|Mod,
//! BinOp::Div, and BinOp::Eq|Neq|Lt|Gt|Leq|Geq arms.

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

// ─────────────────────────────────────────────────────────────────────
// Round 72: Div + ordering-comparison single-diagnostic regression
// locks. Mirror the Add/Sub/Mul/Mod tests above for `/`, `<`, `>`,
// `<=`, `>=`, `==`, `!=`.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn test_bool_div_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = true / 1
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'/'");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true / 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn test_bool_lt_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = (true < 1)
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'<'");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true < 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn test_bool_gt_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = (true > 1)
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'>'");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true > 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn test_bool_leq_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = (true <= 1)
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'<='");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true <= 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn test_bool_geq_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = (true >= 1)
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'>='");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true >= 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

/// Defensive (round 72): Eq/Neq currently get away with the dual-emit
/// because Bool is a valid equality operand and the domain check passes.
/// But the snapshot pattern was applied uniformly — verify the
/// type-mismatch diagnostic still appears exactly once for Bool == Int.
#[test]
fn test_bool_eq_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = (true == 1)
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'=='");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true == 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

#[test]
fn test_bool_neq_int_emits_single_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
    let x = (true != 1)
    ()
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'!='");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for `true != 1`, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
}

/// Ascribed-let variant for Div, mirroring
/// `tests/ascribed_let_binop_single_diagnostic_tests.rs`. Verifies the
/// `Type::Error` cascade-suppression branch (mod.rs:741) catches the
/// outer ascription so the diagnostic isn't re-emitted.
///
/// Round 100: `String` is outside the `/` operand domain while `Int` is
/// inside it, so the lone diagnostic is now the operand-domain message
/// naming the true offender ("operator '/' requires ..., got 'String'")
/// instead of a misdirected "expected Int, got String" mismatch. The
/// single-print invariant is unchanged.
#[test]
fn test_ascribed_let_div_mismatch_prints_once() {
    let errs = type_errors(
        r#"
fn main() {
    let s: String = "hello"
    let n: Int = s / 1
    println(n)
}
"#,
    );
    let (mismatch, domain) = count_binop_diagnostics(&errs, "'/'");
    assert_eq!(
        mismatch + domain,
        1,
        "expected exactly one of (type mismatch | operator-domain) for ascribed-let div, \
         got mismatch={mismatch}, domain={domain}, all errors:\n{}",
        errs.join("\n")
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("operator '/' requires Int, Float, or ExtFloat, got 'String'")),
        "expected the operand-domain diagnostic naming the String offender, got:\n{}",
        errs.join("\n")
    );
}
