//! Phase B locks for the typechecker's annotation-enforcement pass.
//!
//! See `docs/proposals/effect-rows.md` (Part 7). Phase B compares the
//! body's inferred effect set to the declared annotation; if the body
//! computes an effect the signature didn't declare, the typechecker
//! emits a "type" diagnostic naming the offending effect.
//!
//! Compatibility invariants locked here:
//!   - A fn with NO annotation defaults to `EffectSet::TOP` and
//!     accepts any inferred body — every legacy program keeps
//!     typechecking unchanged.
//!   - A fn declared `!{}` (pure) rejects any non-empty inferred set.
//!   - A fn declared `!{io, fs}` accepts a body whose inferred set is
//!     a subset (including narrower or equal sets).
//!   - Inferred wider than declared is the error case; the diagnostic
//!     mentions the offending effect.

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker::{self, Severity};

fn check_src(src: &str) -> Vec<silt::types::TypeError> {
    let tokens = Lexer::new(src).tokenize().expect("lexer ok");
    let mut prog = Parser::new(tokens).parse_program().expect("parse ok");
    typechecker::check(&mut prog)
}

fn has_error_containing(errs: &[silt::types::TypeError], needle: &str) -> bool {
    errs.iter()
        .any(|e| e.severity == Severity::Error && e.message.contains(needle))
}

// ── 1. Pure annotation accepts pure body ───────────────────────────

#[test]
fn pure_annotation_accepts_pure_body() {
    // `!{}` declares fully pure. A body with no calls, no builtins,
    // is inferred as EMPTY → subset of EMPTY → no error.
    let errs = check_src("fn f() -> Int !{} = 42");
    let typed_errs: Vec<_> = errs
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        typed_errs.is_empty(),
        "pure body under !{{}} should typecheck, got: {typed_errs:?}"
    );
}

// ── 2. Pure annotation rejects io body ─────────────────────────────

#[test]
fn pure_annotation_rejects_io_body() {
    // The body calls a stdlib builtin (currently `TOP` under the
    // gradual rollout, so it carries every effect bit). Any non-empty
    // inferred set under `!{}` triggers the diagnostic.
    let errs = check_src(r#"fn f() -> String !{} = io.read_file("x")"#);
    assert!(
        has_error_containing(&errs, "not declared"),
        "io.read_file under !{{}} must error, got: {errs:?}"
    );
}

// ── 3. Wider annotation accepts narrower body ──────────────────────

#[test]
fn io_annotation_accepts_io_body() {
    // The body's inferred set (TOP under Phase A/B-stdlib-still-TOP)
    // is the same as TOP, but the declared `!{io, fs}` annotation —
    // explicitly written — is the *opaque* boundary. A user-declared
    // narrow set with a TOP body is the case covered by test 4.
    //
    // For this test, exercise the simpler case: a body that *only*
    // unions `io` calls (which is still TOP today) under `!{io, fs}`
    // would still surface a `!net` / `!time` / `!random` overflow
    // because builtins are TOP. So we test by writing a body that's
    // pure and declared `!{io, fs}` — `EMPTY ⊆ {io, fs}` accepts.
    let errs = check_src("fn f() -> Int !{io, fs} = 42");
    let typed_errs: Vec<_> = errs
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        typed_errs.is_empty(),
        "pure body under !{{io, fs}} accepts (subset rule), got: {typed_errs:?}"
    );
}

// ── 4. Narrow annotation rejects wider body ────────────────────────

#[test]
fn narrow_annotation_rejects_wider_body() {
    // Body uses `io.read_file` which is gradual-rollout TOP. Declared
    // `!{fs}` is narrower than TOP, so the diagnostic fires. Phase A's
    // `effect_inference_phase_a_tests` lock that `io.read_file` infers
    // TOP today; this test stacks on top: TOP ⊄ {fs} → error.
    let errs = check_src(r#"fn f() -> String !{fs} = io.read_file("x")"#);
    let any_io = errs
        .iter()
        .any(|e| e.severity == Severity::Error && e.message.contains("!{io}"));
    assert!(
        any_io,
        "diagnostic for body wider than !{{fs}} should mention !{{io}}, got: {errs:?}"
    );
}

// ── 5. Top annotation accepts anything ─────────────────────────────

#[test]
fn top_annotation_accepts_anything() {
    // No annotation written → declared defaults to TOP → no
    // enforcement runs → any body is accepted, including one that
    // uses a TOP-default builtin.
    let errs = check_src(r#"fn f() -> String = io.read_file("x")"#);
    let typed_errs: Vec<_> = errs
        .iter()
        .filter(|e| {
            e.severity == Severity::Error
                && (e.message.contains("not declared") || e.message.contains("effect"))
        })
        .collect();
    assert!(
        typed_errs.is_empty(),
        "no annotation → no enforcement, got: {typed_errs:?}"
    );
}

// ── 6. Diagnostic content lock ─────────────────────────────────────

#[test]
fn error_message_lists_offending_effect_and_signature() {
    // Lock the diagnostic shape so future formatting tweaks don't
    // silently degrade what the user sees in the editor.
    let errs = check_src(r#"fn f() -> String !{fs} = io.read_file("x")"#);
    let err = errs
        .iter()
        .find(|e| e.severity == Severity::Error && e.message.contains("not declared"))
        .expect("expected effect-mismatch diagnostic");
    let msg = &err.message;
    // The message should mention:
    //   - the headline (offending) effect rendered with `!{}` syntax
    //   - the fn name
    //   - what the body uses
    //   - what the signature declares
    assert!(
        msg.contains("not declared in fn"),
        "diagnostic must point at the fn it's complaining about: {msg}"
    );
    assert!(msg.contains("'f'"), "diagnostic should name fn 'f': {msg}");
    assert!(
        msg.contains("body uses"),
        "diagnostic should describe the body: {msg}"
    );
    assert!(
        msg.contains("signature declares"),
        "diagnostic should describe the signature: {msg}"
    );
    assert!(
        msg.contains("!{fs}"),
        "diagnostic should render the declared set: {msg}"
    );
}
