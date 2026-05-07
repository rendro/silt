//! Round 77 lock test for LATENT finding EXHAUST-L1 in
//! `src/typechecker/exhaustiveness.rs`.
//!
//! Pre-fix: the `match ty` inside `missing_description` only had arms for
//! `Type::Bool` and `Type::Generic`. `Type::Record` (which the `Generic`
//! branch resolves to after parameter substitution at lines 1151-1175)
//! fell through to the catch-all `_ => "not all patterns are covered"`,
//! so all the substitution work was wasted at the diagnostic layer.
//!
//! Post-fix: a dedicated `Type::Record` arm walks the record's declared
//! fields, builds a per-field column matrix, and recursively asks
//! `missing_description` what each field column is missing. The first
//! field with a substantive sub-witness wins, formatted as
//! `"in <RecName>.<field>: <child witness>"`. Soundness is unchanged
//! (the match was already rejected); the post-fix wording is a strict
//! improvement on diagnostic quality for non-exhaustive record matches.

use silt::typechecker;
use silt::types::Severity;

/// Drive the real typechecker entry point and collect hard-error
/// diagnostic messages — same plumbing other exhaustiveness tests use.
fn hard_errors(input: &str) -> Vec<String> {
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

/// EXHAUST-L1 primary lock: the repro from the issue. A `Pair(Bool, Int)`
/// matched only with `Pair{first: true, second}` is non-exhaustive — the
/// missing case is `first: false`. The post-fix diagnostic must mention
/// either the record name (`Pair`) or the field name (`first`), proving
/// the new `Type::Record` arm fired and the substituted record type was
/// not wasted on the catch-all.
#[test]
fn record_pattern_non_exhaustive_emits_record_aware_witness() {
    let errs = hard_errors(
        r#"
type Pair(a, b) { first: a, second: b }
fn check(p: Pair(Bool, Int)) -> Int = match p { Pair{first: true, second} -> second }
fn main() { check(Pair{first: true, second: 1}) }
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive")),
        "expected a non-exhaustive diagnostic for Pair(Bool, Int) with only \
         a `first: true` arm, got: {errs:?}"
    );
    let msg = errs
        .iter()
        .find(|m| m.contains("non-exhaustive"))
        .expect("non-exhaustive message must be present");
    assert!(
        msg.contains("Pair") || msg.contains("first"),
        "post-fix diagnostic must go beyond the bare 'not all patterns \
         are covered' and mention either the record name 'Pair' or the \
         field name 'first'; got: {msg}"
    );
    // Bare-message lock: the post-fix arm must not return the exact
    // catch-all string (otherwise the substitution work at lines
    // 1151-1175 of exhaustiveness.rs is still wasted). The message may
    // still CONTAIN "not all patterns" as a fallback suffix
    // (`not all patterns of Pair are covered`), so we test for the
    // exact catch-all phrase, not a substring.
    assert!(
        !msg.ends_with("not all patterns are covered"),
        "post-fix diagnostic must not END with the bare catch-all \
         message — that's the EXHAUST-L1 regression. got: {msg}"
    );
}

/// EXHAUST-L1 secondary lock: a non-generic record produces the same
/// record-aware witness shape. This guards against an over-narrow fix
/// that only handles records reached via the `Type::Generic` -> record
/// substitution path; the `Type::Record` arm itself must do the work.
#[test]
fn non_generic_record_pattern_non_exhaustive_emits_field_witness() {
    let errs = hard_errors(
        r#"
type Box { flag: Bool }
fn check(b: Box) -> Int = match b { Box{flag: true} -> 1 }
fn main() { check(Box{flag: true}) }
"#,
    );
    assert!(
        errs.iter().any(|m| m.contains("non-exhaustive")),
        "expected a non-exhaustive diagnostic for Box{{flag:Bool}} with \
         only a `flag: true` arm, got: {errs:?}"
    );
    let msg = errs
        .iter()
        .find(|m| m.contains("non-exhaustive"))
        .expect("non-exhaustive message must be present");
    assert!(
        msg.contains("Box") || msg.contains("flag") || msg.contains("false"),
        "post-fix diagnostic must mention the record name 'Box', the \
         field name 'flag', or the missing 'false' witness; got: {msg}"
    );
}

/// Negative grep lock: the source must not regress to "Type::Record
/// falls through" or revert to a catch-all-only treatment of records
/// inside `missing_description`. We grep for the literal catch-all
/// pattern that shipped pre-fix immediately under the
/// `Type::Generic { ... } else if let Some(rec_info) = ... }` arm. The
/// post-fix file must contain a dedicated `Type::Record` arm above the
/// final catch-all `_ =>`. We assert the source contains the marker
/// comment that documents the round-77 fix.
#[test]
fn exhaustiveness_source_has_record_arm_marker() {
    let src = include_str!("../src/typechecker/exhaustiveness.rs");
    assert!(
        src.contains("EXHAUST-L1"),
        "src/typechecker/exhaustiveness.rs must retain the round-77 \
         EXHAUST-L1 marker that documents the dedicated Type::Record \
         arm in missing_description; if removed, the catch-all bug \
         likely returned"
    );
    // Hard structural lock: the substring `Type::Record(rec_name, rec_fields) =>`
    // must appear inside missing_description's match. If a future
    // refactor accidentally collapses the arm back into the catch-all
    // this assertion fails.
    assert!(
        src.contains("Type::Record(rec_name, rec_fields) =>"),
        "missing_description must keep its dedicated Type::Record arm \
         (round-77 EXHAUST-L1 fix); if this assertion fails, records \
         once again fall through to the bare catch-all"
    );
}
