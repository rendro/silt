//! `when let ... else` with a `match` in the else body, and the
//! withdrawn proposal `docs/proposals/when-let-else-match.md`.
//!
//! The proposal set out to replace this shape:
//!
//! ```silt
//! when let Ok(data) = res else {
//!   match res {
//!     Err(e) -> panic(...)
//!     Ok(_)  -> panic("unreachable")
//!   }
//! }
//! ```
//!
//! The typechecker (src/typechecker/inference.rs, `Stmt::When`)
//! requires the else body to infer `Type::Never`. A `match` whose arms
//! all diverge used not to infer `Never`, so the shape was rejected
//! with `'when let' else body must diverge — use 'return' or 'panic'`.
//! The proposal was withdrawn in favour of closing that gap: a `match`
//! whose every arm has type `Never` has type `Never`, and the shape
//! type checks.
//!
//! These tests lock:
//!
//!   (a) the match-in-else shape typechecks, and a `match` with an arm
//!       that yields a value is still rejected as a non-diverging else
//!       body;
//!   (b) the direct-panic else form typechecks;
//!   (c) the match-before-when-let form typechecks;
//!   (d) the proposal file quotes the diagnostic and does not carry the
//!       old "works, just ugly" claim.

use silt::typechecker;
use silt::types::Severity;

/// Lex + parse + typecheck `input`, returning the Error-severity
/// diagnostic messages. Same helper pattern as
/// `tests/round88_typechecker_doc_and_dead_arm_lock_tests.rs`.
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

/// (a) A `match` whose arms all diverge, inside the `when let` else
/// body. Every arm has type `Never`, so the match has type `Never` and
/// the else body diverges. Must typecheck clean.
#[test]
fn match_in_when_let_else_with_all_arms_diverging_typechecks() {
    let errors = type_errors(
        r#"
fn load_data() -> Result(Int, String) {
  Ok(42)
}

fn main() {
  let res = load_data()
  when let Ok(data) = res else {
    match res {
      Err(e) -> panic("load failed: {e}")
      Ok(_)  -> panic("unreachable")
    }
  }
  println(data)
}
"#,
    );
    assert!(
        errors.is_empty(),
        "a `match` whose arms all diverge has type Never, so it is a \
         diverging `when let` else body; it must typecheck clean. \
         Errors: {errors:?}"
    );
}

/// (a, continued) A `match` with an arm that yields a value does not
/// diverge. As a `when let` else body it is still rejected.
#[test]
fn match_in_when_let_else_with_a_value_arm_is_rejected_as_non_diverging() {
    let errors = type_errors(
        r#"
fn load_data() -> Result(Int, String) {
  Ok(42)
}

fn main() {
  let res = load_data()
  when let Ok(data) = res else {
    match res {
      Err(e) -> panic("load failed: {e}")
      Ok(_)  -> 7
    }
  }
  println(data)
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|m| m.contains("'when let' else body must diverge")),
        "a `match` with an arm that yields a value does not diverge; as \
         a `when let` else body it must be rejected with the \"'when \
         let' else body must diverge\" diagnostic. Errors: {errors:?}"
    );
}

/// (b) The first form the doc shows: diverge directly in the else body
/// with `panic`. Must typecheck clean.
#[test]
fn direct_panic_in_when_let_else_typechecks() {
    let errors = type_errors(
        r#"
fn load_data() -> Result(Int, String) {
  Ok(42)
}

fn main() {
  let res = load_data()
  when let Ok(data) = res else {
    panic("load failed")
  }
  println(data)
}
"#,
    );
    assert!(
        errors.is_empty(),
        "the direct-panic else form is documented in \
         docs/proposals/when-let-else-match.md; it must typecheck \
         clean. Errors: {errors:?}"
    );
}

/// (c) The second form the doc shows: variant-aware `match` BEFORE the
/// `when let` (with its `Ok(_) -> {}` ceremony arm), then a `when let`
/// whose else diverges directly. Must typecheck clean.
#[test]
fn match_before_when_let_workaround_typechecks() {
    let errors = type_errors(
        r#"
fn load_data() -> Result(Int, String) {
  Ok(42)
}

fn main() {
  let res = load_data()
  match res {
    Err(e) -> panic("load failed: {e}")
    Ok(_)  -> {}
  }
  when let Ok(data) = res else {
    panic("unreachable")
  }
  println(data)
}
"#,
    );
    assert!(
        errors.is_empty(),
        "the match-before-when-let form is documented in \
         docs/proposals/when-let-else-match.md; it must typecheck \
         clean. Errors: {errors:?}"
    );
}

/// (d) Lock on the proposal doc itself. The doc is kept as a record of
/// a withdrawn proposal: it quotes the diagnostic, and the claim that
/// the match-in-else shape "works, just ugly" (false when the proposal
/// was written) must not come back into it.
#[test]
fn proposal_doc_quotes_the_diagnostic() {
    let doc = include_str!("../docs/proposals/when-let-else-match.md");

    assert!(
        doc.contains("'when let' else body must diverge"),
        "docs/proposals/when-let-else-match.md should quote the actual \
         diagnostic (\"'when let' else body must diverge\") so users \
         searching for the error find the proposal"
    );
    assert!(
        !doc.contains("works, just ugly"),
        "docs/proposals/when-let-else-match.md regressed to the old \
         \"works, just ugly\" claim about `else {{ match ... }}`, which \
         was false when the proposal was written"
    );
}
