//! Regression tests for the tightened short-pair Levenshtein threshold
//! in `src/typechecker/suggest.rs`.
//!
//! Round-24 audit finding (LATENT): the previous short-pair rule
//! accepted edit distance `<= 2` whenever `max(len) <= 5`. That bucket
//! surfaced low-signal hints like `foo` → `Bool` (distance 2) where the
//! intent is almost certainly nothing of the kind. The fix tightens the
//! short-pair threshold to `d <= 1`; longer pairs keep the scaled
//! `d * 3 <= max` rule, which still catches genuine 1- and 2-char typos
//! on identifiers with meaningful length (`pintln` → `println`,
//! `lenght` → `length`).
//!
//! Assertions (from the audit brief):
//!   - `foo` (undefined) does NOT suggest `Bool`.
//!   - `pintln` (typo) DOES suggest `println`.
//!   - `lenght` (typo for `length`) DOES suggest `length`.
//!   - Short typos with 1 edit still suggest (constructed scenario with
//!     two short locals `to` and `of`; user typos `te`, picks `to`).
//!
//! Tests drive full typechecker runs via the same path as
//! `tests/diagnostic_suggestion_tests.rs` so the call path through
//! `format_undefined_variable_message` → `suggest_similar` is covered.

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

fn first_error_containing(errs: &[String], needle: &str) -> Option<String> {
    errs.iter().find(|e| e.contains(needle)).cloned()
}

// ── Core: `foo` must NOT suggest `Bool` ─────────────────────────────

#[test]
fn test_foo_undefined_does_not_suggest_bool() {
    // `foo` (len 3) vs `Bool` (len 4): edit distance 2, max 4. Under
    // the old short-pair rule (d <= 2 for max <= 5) this produced the
    // canonical low-signal hint `did you mean \`Bool\`?`. The tightened
    // rule (d <= 1 for max <= 5) must suppress it.
    //
    // We deliberately keep the program otherwise empty so `Bool` is the
    // closest candidate and the hint is deterministic. If another
    // suggestion of distance 1 appeared, this test would not be load-
    // bearing for the `Bool` case; there is none at this writing.
    let errs = type_errors(r#"fn main() { foo }"#);
    let msg = first_error_containing(&errs, "undefined variable 'foo'")
        .expect("expected undefined variable error for `foo`");
    assert!(
        !msg.contains("`Bool`"),
        "`foo` must not suggest `Bool` under the tightened threshold, got: {msg:?}"
    );
    // Belt-and-braces: nothing in the default scope is at distance 1
    // from `foo`, so the whole "did you mean" suffix should be absent.
    assert!(
        !msg.contains("did you mean"),
        "`foo` has no 1-edit neighbour — expected no hint at all, got: {msg:?}"
    );
}

// ── Regression: long-pair hints must still fire ─────────────────────

#[test]
fn test_pintln_still_suggests_println() {
    // `pintln` vs `println`: d=1, max=7. Scaled rule: 1*3 <= 7 —
    // accepts. Tightening the short-pair cap must not break this.
    let errs = type_errors(r#"fn main() { pintln("hi") }"#);
    let msg = first_error_containing(&errs, "undefined variable 'pintln'")
        .expect("expected undefined variable error for `pintln`");
    assert!(
        msg.contains("did you mean"),
        "expected 'did you mean' hint for pintln, got: {msg:?}"
    );
    assert!(
        msg.contains("`println`"),
        "expected suggestion `println` for typo `pintln`, got: {msg:?}"
    );
}

#[test]
fn test_lenght_still_suggests_length() {
    // `lenght` vs `length`: d=2, max=6. Scaled rule: 2*3 <= 6 — accepts
    // at the boundary. This is the textbook transpose typo and the
    // test that locks the scaled rule against regressions.
    let errs = type_errors(
        r#"
import list
fn main() {
  let xs = [1, 2, 3]
  list.lenght(xs)
}
"#,
    );
    let msg = first_error_containing(&errs, "unknown function 'lenght'")
        .expect("expected unknown module function error for `lenght`");
    assert!(
        msg.contains("did you mean"),
        "expected 'did you mean' hint for lenght, got: {msg:?}"
    );
    assert!(
        msg.contains("`length`"),
        "expected suggestion `length` for typo `lenght`, got: {msg:?}"
    );
}

// ── Regression: short 1-edit typos still suggest ────────────────────

#[test]
fn test_short_one_edit_typo_still_suggests() {
    // Two short 2-char locals `to` and `of`. User typos `te` — edit
    // distance 1 from `to` (substitute), 2 from `of`. Under the
    // tightened rule (d <= 1 for max <= 5) the hint must still fire
    // for the 1-edit neighbour. This locks the fact that we tightened
    // the *ceiling* on short pairs, not the *floor*: 1-edit typos on
    // very short names must keep working.
    let errs = type_errors(
        r#"
fn main() {
  let to = 1
  let of = 2
  te
}
"#,
    );
    let msg = first_error_containing(&errs, "undefined variable 'te'")
        .expect("expected undefined variable error for `te`");
    assert!(
        msg.contains("did you mean"),
        "expected 'did you mean' hint for short 1-edit typo, got: {msg:?}"
    );
    assert!(
        msg.contains("`to`"),
        "expected suggestion `to` (d=1) for typo `te`, got: {msg:?}"
    );
    // And the 2-edit neighbour `of` must NOT win: same tightening that
    // killed `foo` → `Bool` also rules out `te` → `of`.
    assert!(
        !msg.contains("`of`"),
        "`of` is d=2 from `te` — tightened rule must not offer it, got: {msg:?}"
    );
}

// ── Short 2-edit noise (the thing we were fixing) ───────────────────

#[test]
fn test_short_two_edit_noise_is_suppressed() {
    // Local `cat` is 2 edits from the typo `dog` (max=3, d=3 actually —
    // too far either way). Use `cat` vs `bat`: d=1 (still suggests —
    // this is the *control*). Then use `cat` vs `dog`: d=3 — too far
    // under any rule. To isolate the 2-edit case we need d=2 with
    // max<=5: `cat` vs `cab` is d=1, not 2. Use `cat` vs `cup`: d=2,
    // max=3 — under old rule this suggested, under new rule it must
    // not. Program: local `cat`, typo `cup`.
    let errs = type_errors(
        r#"
fn main() {
  let cat = 1
  cup
}
"#,
    );
    let msg = first_error_containing(&errs, "undefined variable 'cup'")
        .expect("expected undefined variable error for `cup`");
    // No 1-edit neighbour for `cup` in the local scope; `cat` is d=2,
    // which the tightened rule rejects. Expect no hint at all.
    assert!(
        !msg.contains("`cat`"),
        "short-pair d=2 noise (`cup` -> `cat`) must be suppressed, got: {msg:?}"
    );
}
