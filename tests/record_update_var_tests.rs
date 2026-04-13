//! Regression tests for round-23 finding #2: RecordUpdate on a bare
//! Var receiver silently accepted typoed field names.
//!
//! Before the fix, `fn f(r, n) { r.{ aeg: n } }` type-checked clean.
//! After inference narrowed `r` to a concrete `Rec` record via the
//! call site, the typo `aeg` was simply ignored — at runtime the VM
//! either panicked ("unknown field") or, worse, the updated record
//! was dropped on the floor and the caller saw the unmodified value.
//!
//! The fix piggy-backs on the existing B4 `pending_field_accesses`
//! machinery: each field in a RecordUpdate against a Var receiver is
//! deferred and re-validated once the receiver is narrowed. If the
//! narrowed base type does not declare the field, we emit the same
//! "unknown field on type <T>" diagnostic that normal FieldAccess uses.
//!
//! These tests assert on the raw typechecker error messages. The first
//! test locks the real typo case; the second guards against
//! false-positives on valid field updates through a polymorphic param.

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
fn test_record_update_on_var_receiver_flags_typoed_field() {
    // The round-23 repro. `update_age`'s param `r` surfaces as a bare
    // Var during body inference; the call site pins it to Rec; then
    // the deferred pending_field_access entry for `aeg` fires at
    // finalize with "unknown field ... on type Rec".
    let errs = type_errors(
        r#"
type Rec { name: String, age: Int }
fn update_age(r, new_age) { r.{ aeg: new_age } }
fn main() {
  let r = Rec { name: "alice", age: 30 }
  println(update_age(r, 31).age)
}
"#,
    );
    let hit = errs
        .iter()
        .any(|m| m.contains("unknown field") && m.contains("aeg"));
    assert!(
        hit,
        "expected an 'unknown field ... aeg' diagnostic, got: {errs:?}"
    );
}

#[test]
fn test_record_update_on_var_receiver_valid_field_type_checks_clean() {
    // Regression: the fix must not flag a *valid* field update on a
    // polymorphic-looking param. Once the call site narrows `r` to
    // Rec, `age` is a known field and the deferred check unifies the
    // result-type with Int.
    let errs = type_errors(
        r#"
type Rec { name: String, age: Int }
fn bump(r, n) { r.{ age: n } }
fn main() {
  let r = Rec { name: "alice", age: 30 }
  println(bump(r, 31).age)
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected zero errors for valid field update through fn param, got: {errs:?}"
    );
}
