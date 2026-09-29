//! Round 92: end-to-end behavioral lock for the round-91 `silt test`
//! import-cascade filter fix (commit 49bfdb5, src/cli/test.rs:258).
//!
//! Round 91 made the `silt test` type-error loop route through the
//! shared `should_suppress_import_cascade` predicate instead of the old
//! warning-only `is_unknown_module_warning(&source_err)` filter. The
//! round-91 lock tests pinned only the PREDICATE
//! (`silt::diagnostic_filters::should_suppress_import_cascade_message`),
//! not the WIRING — reverting test.rs to the old filter left the whole
//! suite green. This file locks the wiring behaviorally by running the
//! compiled `silt` binary on temp packages.
//!
//! The deterministic distinguishing scenario: a test file that
//! selectively imports items from a user module the compiler cannot
//! load (`import ghost_mod.{answer}` with no ghost_mod.silt on disk).
//! The typechecker then emits exactly the round-91 cascade shape
//! (verified library-level below):
//!
//!   Warning "unknown module 'ghost_mod'; imported items will not be type-checked"
//!   Error   "undefined variable 'answer'"
//!
//! PRE-fix `silt test` skipped only the warning, printed the
//! "undefined variable" cascade, emitted
//! "failed to compile — type errors (see above)", and `continue`d —
//! never reaching the compile pass, so the GENUINE root cause
//! ("cannot load module 'ghost_mod'") was never shown. POST-fix the
//! cascade is suppressed, the compile pass runs, and the genuine
//! module-load error surfaces — the same diagnostic `silt run` shows
//! (parity, asserted below). So the assertions here fail decisively
//! under the pre-fix wiring.
//!
//! Note on the fully-green parity direction (test PASSES where the
//! typechecker can't see into a module but the compiler links it):
//! that needs partial module-resolution state — the pre-typecheck
//! pass (`Compiler::pre_typecheck_imports`) populates exports for any
//! readable+lexable module, including one whose own imports are
//! missing, so no cascade arises in that shape. Round 91 already
//! documented that as hard to construct deterministically; the
//! error-surfacing direction locked here exercises the same wiring
//! (the `should_suppress_import_cascade(...)` branch must fire for
//! both the warning AND the cascade error for these tests to pass).
//!
//! The end-to-end `silt test` / `silt run` checks are golden cases in
//! tests/golden/lang/modules/round92_test_import_filter_e2e__*. The shape
//! lock below calls the typechecker's library API, so it stays here. The
//! golden cases carry the same ghost-module test source as GHOST_TEST_SRC.

use std::collections::HashMap;

/// Test-file source whose selective import cannot be loaded by the
/// compiler (no ghost_mod.silt is ever written next to it).
const GHOST_TEST_SRC: &str = r#"import test
import ghost_mod.{answer}

fn test_answer() {
    test.assert_eq(answer() + 1, 42)
}
"#;

// ── Shape lock: the scenario really produces the round-91 cascade ──

/// Library-level guard that the ghost-module scenario produces EXACTLY
/// the round-91 diagnostic shape the E2E tests below depend on: the
/// "unknown module" warning plus an "undefined variable" cascade
/// error. If the typechecker ever stops emitting this pair, the E2E
/// locks below would lose their distinguishing power silently — this
/// test makes that drift loud instead.
#[test]
fn ghost_module_scenario_produces_round91_cascade_shape() {
    let tokens = silt::lexer::Lexer::new(GHOST_TEST_SRC)
        .tokenize()
        .expect("test source must lex");
    let (mut program, parse_errors) = silt::parser::Parser::new(tokens).parse_program_recovering();
    assert!(
        parse_errors.is_empty(),
        "test source must parse cleanly, got: {parse_errors:?}"
    );
    // Empty exports map = the typechecker cannot see into ghost_mod,
    // exactly what `silt test` computes when the pre-typecheck pass
    // fails to load the module file.
    let (type_errors, _exports) = silt::typechecker::check_with_package_and_imports_options(
        &mut program,
        None,
        HashMap::new(),
        false,
    );
    assert!(
        type_errors
            .iter()
            .any(|te| te.severity == silt::typechecker::Severity::Warning
                && te.message.contains("unknown module 'ghost_mod'")),
        "expected the unknown-module warning in the raw diagnostics, got: {:?}",
        type_errors.iter().map(|te| &te.message).collect::<Vec<_>>()
    );
    assert!(
        type_errors
            .iter()
            .any(|te| te.severity == silt::typechecker::Severity::Error
                && te.message.starts_with("undefined variable 'answer'")),
        "expected the undefined-variable cascade error in the raw diagnostics, got: {:?}",
        type_errors.iter().map(|te| &te.message).collect::<Vec<_>>()
    );
}
