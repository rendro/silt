//! Regression tests for round-23 finding #4: record-type used as
//! tuple-constructor pattern produced a nonsense "constructor expects
//! 0 fields" error.
//!
//! When the user writes `match c { Circle(r) -> ... }` and `Circle`
//! is a record type (not an enum variant), the old diagnostic said
//! "constructor 'Circle' expects 0 fields, but pattern has 1" — but
//! records *do* have fields, they just use `Circle { radius: r }`
//! pattern syntax. The fix detects the record-vs-variant case at the
//! default arm of `check_pattern` and emits a shape-pointing hint.

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
fn test_record_type_as_ctor_pattern_points_at_record_syntax() {
    // The round-23 repro. `Circle(r)` against a record `Circle`.
    let errs = type_errors(
        r#"
type Circle { radius: Int }
fn main() {
  let c = Circle { radius: 5 }
  match c { Circle(r) -> println(r) }
}
"#,
    );
    // The new message names the type and points at record-pattern
    // syntax. We don't pin the exact wording, but assert both pieces
    // of information are present so the user can act on it.
    let hit = errs.iter().any(|m| {
        m.contains("record")
            && m.contains("Circle")
            && !m.contains("expects 0 fields")
    });
    assert!(
        hit,
        "expected a record-pattern hint (and NOT the legacy 'expects 0 fields' text), got: {errs:?}"
    );
}

#[test]
fn test_enum_variant_arity_mismatch_still_uses_legacy_wording() {
    // Regression: the fix must not accidentally swallow the real
    // "expects N fields, but pattern has M" arity error for enum
    // variants with the wrong sub-pattern count.
    let errs = type_errors(
        r#"
type Shape { Circle(Int), Square }
fn main() {
  let c = Circle(5)
  match c {
    Circle(r, extra) -> println(r)
    Square -> println("sq")
  }
}
"#,
    );
    let hit = errs
        .iter()
        .any(|m| m.contains("constructor") && m.contains("expects"));
    assert!(
        hit,
        "expected 'constructor ... expects N fields' wording for variant arity mismatch, got: {errs:?}"
    );
}
