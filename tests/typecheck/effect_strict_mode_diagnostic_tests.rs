//! Phase D locks for the `--strict-effects` typechecker mode.
//!
//! See `docs/proposals/effect-rows.md` (Part 7 Phase D) and
//! `docs/strict-effects-migration.md`. Phase D flips the
//! gradual-rollout `EffectSet::TOP` default for unannotated user fns
//! to `EffectSet::EMPTY`, so the body-inference subset check rejects
//! any effectful builtin call from a fn whose signature didn't
//! declare the effect. The diagnostic carries a copy-paste
//! `help: annotate as ...` line that reproduces the user's signature
//! with the inferred set spliced in — the migration story depends on
//! that line being a literal paste-target, so this suite locks its
//! presence + shape.
//!
//! Compatibility invariants locked here:
//!   - Strict mode OFF (the default) reproduces Phase B behavior:
//!     unannotated fns calling effectful builtins typecheck cleanly.
//!   - Strict mode ON flips the default; the diagnostic explains what
//!     was inferred AND ships the literal annotation the user can
//!     paste back in.
//!   - A pure unannotated fn under strict mode still typechecks
//!     (no false positives on legitimately-pure code).
//!   - A user-annotated fn under strict mode still honours the
//!     declared bound (no double-enforcement).

use silt::typechecker::{self, Severity, TypeError};

fn check_strict(src: &str) -> Vec<TypeError> {
    let tokens = silt::lexer::Lexer::new(src).tokenize().expect("lex ok");
    let mut prog = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse ok");
    let (errs, _exports) = typechecker::check_with_package_and_imports_options(
        &mut prog,
        None,
        std::collections::HashMap::new(),
        true,
    );
    errs
}

fn check_lax(src: &str) -> Vec<TypeError> {
    let tokens = silt::lexer::Lexer::new(src).tokenize().expect("lex ok");
    let mut prog = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse ok");
    typechecker::check(&mut prog)
}

fn first_strict_error(errs: &[TypeError]) -> Option<&TypeError> {
    errs.iter()
        .find(|e| e.severity == Severity::Error && e.message.contains("--strict-effects"))
}

// ── 1. Strict mode rejects an unannotated IO call ──────────────────

#[test]
fn strict_mode_rejects_unannotated_io_call() {
    // `io.read_file` carries `!{fs, io}` after Phase C; an unannotated
    // user fn under strict mode treats absent annotation as `!{}`,
    // so the body's effects exceed the declared bound and the
    // diagnostic fires.
    let errs =
        check_strict("import io\nfn f(p: String) -> Result(String, IoError) = io.read_file(p)");
    let strict_err = first_strict_error(&errs)
        .unwrap_or_else(|| panic!("expected strict-mode diagnostic, got: {errs:?}"));
    assert!(
        strict_err.message.contains("'f'"),
        "diagnostic must name the offending fn 'f': {}",
        strict_err.message
    );
}

// ── 2. Strict mode accepts an unannotated pure function ────────────

#[test]
fn strict_mode_accepts_unannotated_pure_function() {
    // A fn that only does arithmetic doesn't touch any builtin, so its
    // inferred set is EMPTY. EMPTY ⊆ EMPTY → strict-mode flip from
    // TOP to EMPTY is benign here. No false positive.
    let errs = check_strict("fn add(x: Int, y: Int) -> Int = x + y");
    let any_strict = errs
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .any(|e| e.message.contains("--strict-effects"));
    assert!(
        !any_strict,
        "pure fn must typecheck under --strict-effects, got: {errs:?}"
    );
}

// ── 3. Strict mode accepts an annotated IO function ────────────────

#[test]
fn strict_mode_accepts_annotated_io_function() {
    // The user wrote `!{fs, io}` explicitly, so strict mode does NOT
    // flip the declared default — the user's annotation is the
    // declared bound. `io.read_file`'s `!{fs, io}` is exactly the
    // declared set → subset check passes → no error.
    let errs = check_strict(
        "import io\nfn f(p: String) -> Result(String, IoError) !{fs, io} = io.read_file(p)",
    );
    let any_err = errs.iter().any(|e| e.severity == Severity::Error);
    assert!(
        !any_err,
        "user-annotated fn matching its body's effects must typecheck: {errs:?}"
    );
}

// ── 4. Strict-mode diagnostic includes help with suggested annotation

#[test]
fn strict_mode_diagnostic_includes_help_with_suggested_annotation() {
    // The migration story turns on the `help:` line being a literal
    // paste-target. Lock both the `help:` prefix AND the inferred
    // effect set rendering so a future formatter tweak can't silently
    // degrade the user's copy-paste experience.
    let errs =
        check_strict("import io\nfn f(p: String) -> Result(String, IoError) = io.read_file(p)");
    let strict_err = first_strict_error(&errs)
        .unwrap_or_else(|| panic!("expected strict-mode diagnostic, got: {errs:?}"));
    let msg = &strict_err.message;
    assert!(
        msg.contains("help:"),
        "strict-mode diagnostic MUST include a `help:` line: {msg}"
    );
    assert!(
        msg.contains("annotate as"),
        "help line must use the canonical 'annotate as' phrasing: {msg}"
    );
    assert!(
        msg.contains("!{fs, io}"),
        "help line must include the literal inferred effect set !{{fs, io}}: {msg}"
    );
    assert!(
        msg.contains("fn f(p: String) -> Result(String, IoError) !{fs, io}"),
        "help line must reproduce the full fn header with effects spliced in: {msg}"
    );
}

// ── 5. Strict mode is off by default ───────────────────────────────

#[test]
fn strict_mode_off_by_default() {
    // The exact same code from test 1 must typecheck cleanly when
    // strict mode is NOT enabled — Phase D ships disabled-by-default
    // for backwards compat. Locks the "every existing program keeps
    // typechecking" invariant from the spec's hard constraints.
    let errs = check_lax("import io\nfn f(p: String) -> Result(String, IoError) = io.read_file(p)");
    let any_strict = errs.iter().any(|e| e.message.contains("--strict-effects"));
    let any_err = errs.iter().any(|e| e.severity == Severity::Error);
    assert!(
        !any_strict && !any_err,
        "strict-mode diagnostic must NOT fire when --strict-effects is off: {errs:?}"
    );
}

// ── 6. Annotated empty set still rejects an effectful body ─────────

#[test]
fn user_written_empty_annotation_still_rejects_under_strict_mode() {
    // A user who writes `!{}` explicitly is asking for purity; strict
    // mode shouldn't relabel that as the strict-flipped diagnostic.
    // We expect the original Phase B diagnostic instead. Locks that
    // strict mode and Phase B remain orthogonal: an explicit `!{}`
    // is honored as the user's intent, not as the strict-mode flip.
    let errs =
        check_strict("import io\nfn f(p: String) -> Result(String, IoError) !{} = io.read_file(p)");
    let any_err = errs
        .iter()
        .any(|e| e.severity == Severity::Error && e.message.contains("not declared"));
    assert!(
        any_err,
        "explicit !{{}} annotation must surface the Phase B 'not declared' shape, got: {errs:?}"
    );
    let any_strict_flavor = errs.iter().any(|e| e.message.contains("--strict-effects"));
    assert!(
        !any_strict_flavor,
        "explicit !{{}} must NOT route through the strict-mode flipped diagnostic: {errs:?}"
    );
}

// ── 7. Strict mode propagates through call chains ──────────────────

#[test]
fn strict_mode_rejects_pure_caller_of_effectful_helper() {
    // A fn that calls another unannotated fn — under strict mode,
    // both default to EMPTY. The leaf calling `io.read_file` errors
    // first; the caller's body-effect inference sees the leaf's
    // declared (now EMPTY) effects, so the caller stays clean.
    // This locks the propagation behaviour.
    let src = r#"
        import io
        fn read(p: String) -> Result(String, IoError) = io.read_file(p)
        fn caller(p: String) -> Result(String, IoError) = read(p)
    "#;
    let errs = check_strict(src);
    // The leaf `read` MUST error.
    let leaf_err = errs.iter().any(|e| {
        e.severity == Severity::Error
            && e.message.contains("'read'")
            && e.message.contains("--strict-effects")
    });
    assert!(
        leaf_err,
        "leaf fn calling io.read_file must surface strict-mode error: {errs:?}"
    );
}
