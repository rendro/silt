//! Round 62 G4 regression lock.
//!
//! `check_unresolved_in_block` (and its top-level analogue
//! `check_unresolved_let_types`) used to emit a "cannot infer the type of
//! `x`" diagnostic whenever the value expression resolved to a bare
//! `Type::Var`. When the value itself had already failed to typecheck —
//! e.g. `let x = nonExistent` after the undefined-ident error — the cascade
//! diagnostic was misleading: the user is told to add an annotation that
//! wouldn't help, because fixing the undefined name would also resolve the
//! inference.
//!
//! The fix in `resolve.rs` (`value_already_errored`) skips the cascade when
//! `self.errors` already contains a diagnostic whose span points at any
//! sub-expression of the let's value.
//!
//! These tests lock the behaviour:
//!   - undefined-ident value → only the undefined-ident diagnostic
//!   - undefined-module call value → only the module-call diagnostic
//!   - non-erroring polymorphic value → still emits "cannot infer"
//!     (sanity check that the fix is "skip cascade when value errored",
//!     not "delete the diagnostic outright").

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

/// Core repro from the audit finding: `let x = nonExistent` should produce
/// exactly ONE diagnostic — the undefined-variable error — and not the
/// "cannot infer" cascade.
#[test]
fn unused_let_with_undefined_value_emits_only_undefined_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
  let x = nonExistent
}
"#,
    );
    assert_eq!(
        errs.len(),
        1,
        "expected exactly one diagnostic, got {}:\n{}",
        errs.len(),
        errs.join("\n")
    );
    let only = &errs[0];
    assert!(
        only.contains("undefined variable") && only.contains("nonExistent"),
        "expected the diagnostic to be the undefined-variable error, got: {only}"
    );
    assert!(
        !only.contains("cannot infer"),
        "the surviving diagnostic should not be the cascade message: {only}"
    );
}

/// Variant: the value is a call into an unimported / unknown module, which
/// triggers a different diagnostic but should still suppress the
/// "cannot infer" cascade. The exact wording of the module diagnostic is
/// not asserted (it varies between "undefined variable" and module-gating
/// suggestions); what we lock is "no `cannot infer` cascade".
#[test]
fn unused_let_with_unknown_module_call_emits_only_one_diagnostic() {
    let errs = type_errors(
        r#"
fn main() {
  let x = unknown_module.foo()
}
"#,
    );
    let cascade_count = errs.iter().filter(|e| e.contains("cannot infer")).count();
    assert_eq!(
        cascade_count,
        0,
        "did not expect any 'cannot infer' cascade diagnostic when the \
         value already errored, but got:\n{}",
        errs.join("\n")
    );
    // Sanity: at least one diagnostic was emitted (the unknown module / call).
    assert!(
        !errs.is_empty(),
        "expected at least one diagnostic for the unknown-module call"
    );
}

/// Sanity counterpart: a value that typechecks successfully but yields a
/// genuinely polymorphic / unanchored type should STILL emit the
/// "cannot infer" diagnostic. This locks the fix to "skip cascade when
/// value errored" rather than "delete the diagnostic outright".
///
/// `default()` is a zero-arg generic function whose return type is the
/// free type variable `a`, so the call site produces a bare `Type::Var`
/// with no usage to anchor it — exactly the case the "cannot infer"
/// message exists to flag. Critically, `default()` itself typechecks
/// without error, so `value_already_errored` returns false and the
/// cascade DOES fire.
#[test]
fn unused_let_with_no_value_error_still_emits_cannot_infer() {
    let errs = type_errors(
        r#"
fn default() -> a { panic("no value") }
fn main() {
  let x = default()
}
"#,
    );
    let cascade_count = errs.iter().filter(|e| e.contains("cannot infer")).count();
    assert_eq!(
        cascade_count,
        1,
        "expected exactly one 'cannot infer' diagnostic for an unanchored \
         polymorphic value, got {cascade_count} across:\n{}",
        errs.join("\n")
    );
}
