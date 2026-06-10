//! Round 93 BROKEN-doc lock — `docs/proposals/when-let-else-match.md`.
//!
//! The proposal previously presented this shape as what "a user writes
//! today" (and later claimed it "works, just ugly"):
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
//! Reality: the typechecker (src/typechecker/inference.rs, `Stmt::When`)
//! requires the else body to infer `Type::Never`, and a `match` whose
//! arms all panic does NOT infer `Never`. The shape is rejected with
//! `'when let' else body must diverge — use 'return' or 'panic'`.
//! Round 93 rewrote the proposal to say exactly that (the inexpressible
//! shape STRENGTHENS the proposal's motivation) and to show the two
//! real workarounds: a direct `panic` in the else, or a `match` before
//! the `when let`.
//!
//! These tests lock the doc's new claims to actual typechecker
//! behavior:
//!
//!   (a) the match-in-else shape produces the "must diverge"
//!       diagnostic — if the typechecker ever learns to infer `Never`
//!       through `match` (making the doc's OLD claim true), this test
//!       fails and the proposal doc must be revisited;
//!   (b) the direct-panic else form typechecks — the workaround the
//!       doc now recommends really works;
//!   (c) the match-before-when-let form typechecks — the second
//!       documented workaround really works;
//!   (d) source-grep: the proposal file still carries the corrected
//!       wording (and not the old "works, just ugly" claim), so the
//!       doc and the behavioral locks travel together.

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

/// (a) The shape the proposal doc shows as inexpressible today: a
/// `match` (all arms panicking) inside the `when let` else body. Must
/// be rejected with the "must diverge" diagnostic. If this starts
/// passing, the typechecker has learned to infer `Never` through
/// `match` — revisit docs/proposals/when-let-else-match.md, whose
/// Problem section claims the shape "is rejected by the typechecker
/// today".
#[test]
fn match_in_when_let_else_is_rejected_as_non_diverging() {
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
        errors
            .iter()
            .any(|m| m.contains("'when let' else body must diverge")),
        "expected the match-in-else shape to be rejected with the \
         \"'when let' else body must diverge\" diagnostic (a `match` \
         whose arms all panic does not infer Never today). If the \
         typechecker now propagates Never through match, update \
         docs/proposals/when-let-else-match.md — its Problem section \
         claims this shape cannot be written today. Errors: {errors:?}"
    );
}

/// (b) The first workaround the doc now shows: diverge directly in the
/// else body with `panic`. Must typecheck clean.
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
         docs/proposals/when-let-else-match.md as the working \
         status-quo workaround; it must typecheck clean. Errors: {errors:?}"
    );
}

/// (c) The second workaround the doc now shows: variant-aware `match`
/// BEFORE the `when let` (with its `Ok(_) -> {}` ceremony arm), then a
/// `when let` whose else diverges directly. Must typecheck clean.
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
         docs/proposals/when-let-else-match.md as the variant-aware \
         status-quo workaround; it must typecheck clean. Errors: {errors:?}"
    );
}

/// (d) Source-grep lock on the proposal doc itself: the corrected
/// wording must stay, and the old false claim must not come back.
#[test]
fn proposal_doc_carries_corrected_status_quo_wording() {
    let doc = include_str!("../docs/proposals/when-let-else-match.md");

    assert!(
        doc.contains("rejected by the typechecker today"),
        "docs/proposals/when-let-else-match.md no longer says the \
         match-in-else shape is \"rejected by the typechecker today\" — \
         if the typechecker changed, update this whole test file \
         alongside the doc; if the doc was reworded, keep an equivalent \
         honest statement of the status quo"
    );
    assert!(
        doc.contains("'when let' else body must diverge"),
        "docs/proposals/when-let-else-match.md should quote the actual \
         diagnostic (\"'when let' else body must diverge\") so users \
         searching for the error find the proposal"
    );
    assert!(
        !doc.contains("works, just ugly"),
        "docs/proposals/when-let-else-match.md regressed to the old \
         \"works, just ugly\" claim about `else {{ match ... }}` — that \
         shape does NOT typecheck today (see \
         match_in_when_let_else_is_rejected_as_non_diverging)"
    );
}
