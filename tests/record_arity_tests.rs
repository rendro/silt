//! Round 60 B2 regression lock.
//!
//! `resolve_type_expr`'s Generic arm looked up arity in
//! `record_param_var_ids`, which only tracks *parameterized* records
//! (the insert-gate at `mod.rs:1935` skips `td.params.is_empty()`).
//! Arity-0 records therefore got `expected_arity = None` and the arity
//! check was skipped, so `Point(Bool)` against a `type Point { x: Int }`
//! silently became `Type::Generic("Point", [Bool])` and ran. The
//! Record↔Generic unify arms in `unify` no-op'd when
//! `param_var_ids` was absent, completing the silent-acceptance path.
//!
//! The fix chains `record_param_var_ids` onto
//! `.or_else(|| self.records.contains_key(name).then_some(0))` so the
//! arity-0 case emits the standard "expected 0, got N" diagnostic, and
//! mirrors the rejection into the two Record/Generic unify arms.

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

/// Core repro from the audit finding: `Point(Bool)` against a
/// parameterless `Point`. Pre-fix: silently typechecked. Post-fix:
/// arity diagnostic.
#[test]
fn test_parameterless_record_rejects_generic_args() {
    let errs = type_errors(
        r#"
type Point { x: Int, y: Int }
fn read(p: Point(Bool)) -> Int { p.x }
fn main() {
    let p = Point { x: 1, y: 2 }
    let _ = read(p)
}
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("arity")
            || joined.contains("expected 0 type argument")
            || joined.contains("expected 0, got"),
        "expected an arity diagnostic mentioning 'expected 0', got:\n{joined}"
    );
}

/// Bare parameterless record continues to typecheck.
#[test]
fn test_parameterless_record_bare_form_accepted() {
    let errs = type_errors(
        r#"
type Point { x: Int, y: Int }
fn read(p: Point) -> Int { p.x }
fn main() {
    let p = Point { x: 1, y: 2 }
    let _ = read(p)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "bare `Point` should typecheck, got:\n{}",
        errs.join("\n")
    );
}
