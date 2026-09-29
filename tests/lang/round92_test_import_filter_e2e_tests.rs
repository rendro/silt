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

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Unique temp dir per call so parallel tests never share package state.
fn temp_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("silt_round92_import_e2e_{prefix}_{pid}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(dir: &PathBuf, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

/// Run the compiled silt binary with the given subcommand + file path.
fn silt(subcommand: &str, file: &PathBuf) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(subcommand)
        .arg(file)
        .output()
        .expect("failed to spawn silt binary")
}

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

// ── E2E: cascade suppressed, genuine error surfaced ─────────────────

/// THE round-92 wiring lock. Pre-fix wiring (warning-only filter)
/// printed "undefined variable 'answer'" + "failed to compile — type
/// errors (see above)" and never reached the compile pass; post-fix
/// wiring suppresses the cascade and surfaces the genuine
/// "cannot load module 'ghost_mod'" compile error.
#[test]
fn silt_test_suppresses_cascade_and_surfaces_genuine_module_error() {
    let dir = temp_dir("ghost");
    let test_file = write_file(&dir, "app_test.silt", GHOST_TEST_SRC);

    let output = silt("test", &test_file);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "a test file importing an unloadable module must fail; stderr: {stderr}"
    );
    // The genuine root cause MUST surface. Pre-fix this never appeared
    // because the leaked cascade aborted the file before the compile pass.
    assert!(
        stderr.contains("cannot load module 'ghost_mod'"),
        "expected the genuine module-load error to surface (post-round-91 behavior), \
         got stderr: {stderr}"
    );
    // The cascade MUST be suppressed (this is the round-91 fix itself).
    assert!(
        !stderr.contains("undefined variable"),
        "the undefined-variable import cascade leaked into `silt test` output — \
         the round-91 fix (should_suppress_import_cascade wiring in \
         src/cli/test.rs) has regressed. stderr: {stderr}"
    );
    // The unknown-module warning must be suppressed too (both halves of
    // the shared predicate).
    assert!(
        !stderr.contains("unknown module"),
        "the unknown-module warning leaked into `silt test` output. stderr: {stderr}"
    );
    // And the pre-fix failure banner must not appear — the file fails in
    // the COMPILE pass, not the typecheck pass.
    assert!(
        !stderr.contains("failed to compile — type errors"),
        "`silt test` aborted on leaked type errors instead of reaching the \
         compile pass — pre-round-91 behavior. stderr: {stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Parity: `silt run` on the equivalent main-file shows the same
/// genuine root cause and no cascade. Round 91's whole point was that
/// `silt test` must match this behavior, so assert both sides here.
#[test]
fn silt_test_matches_silt_run_on_unresolvable_import() {
    let dir = temp_dir("parity");
    let main_file = write_file(
        &dir,
        "main.silt",
        r#"import ghost_mod.{answer}

fn main() {
    println(answer() + 1)
}
"#,
    );
    let test_file = write_file(&dir, "app_test.silt", GHOST_TEST_SRC);

    let run_out = silt("run", &main_file);
    let run_stderr = String::from_utf8_lossy(&run_out.stderr);
    let test_out = silt("test", &test_file);
    let test_stderr = String::from_utf8_lossy(&test_out.stderr);

    for (label, stderr) in [("silt run", &run_stderr), ("silt test", &test_stderr)] {
        assert!(
            stderr.contains("cannot load module 'ghost_mod'"),
            "{label} must report the genuine module-load root cause; stderr: {stderr}"
        );
        assert!(
            !stderr.contains("undefined variable"),
            "{label} must suppress the import cascade; stderr: {stderr}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

// ── E2E: the round-92 finding's prescribed transitive shape ─────────

/// A test file imports a sibling user module that ITSELF imports a
/// missing module. The pre-typecheck pass still exports the sibling's
/// functions (it swallows the transitive failure), so no typecheck
/// cascade arises in this shape — pre- and post-fix output agree here.
/// Locked anyway as the behavioral contract for the transitive case:
/// the genuine transitive root cause surfaces, with no cascade noise
/// and no "type errors" abort, and suppression never hides a genuine
/// compile error (guards against future over-suppression).
#[test]
fn silt_test_surfaces_transitive_missing_module_not_cascade() {
    let dir = temp_dir("transitive");
    write_file(
        &dir,
        "helper.silt",
        r#"import missing_module_xyz

pub fn answer() -> Int {
  41
}
"#,
    );
    let test_file = write_file(
        &dir,
        "app_test.silt",
        r#"import test
import helper

fn test_answer() {
    test.assert_eq(helper.answer() + 1, 42)
}
"#,
    );

    let output = silt("test", &test_file);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "a transitively-broken import must fail the file; stderr: {stderr}"
    );
    assert!(
        stderr.contains("cannot load module 'missing_module_xyz'"),
        "expected the transitive missing-module root cause to surface; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("undefined variable"),
        "no import cascade may leak for the transitive shape; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("failed to compile — type errors"),
        "the transitive shape must fail in the compile pass, not on leaked \
         type errors; stderr: {stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ── Positive control ────────────────────────────────────────────────

/// When the sibling import resolves cleanly, the test runs and passes.
/// This proves the temp-package harness used by the negative tests
/// above actually resolves sibling imports (so their "no cascade"
/// assertions aren't passing vacuously on a broken setup).
#[test]
fn silt_test_passes_when_sibling_import_resolves() {
    let dir = temp_dir("positive");
    write_file(
        &dir,
        "helper.silt",
        r#"pub fn answer() -> Int {
  41
}
"#,
    );
    let test_file = write_file(
        &dir,
        "app_test.silt",
        r#"import test
import helper

fn test_answer() {
    test.assert_eq(helper.answer() + 1, 42)
}
"#,
    );

    let output = silt("test", &test_file);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "clean sibling import must pass; stderr: {stderr}"
    );
    // The test runner prints PASS lines and the summary to stderr.
    assert!(
        stderr.contains("test_answer") && stderr.contains("1 passed"),
        "expected the test to run and pass; stderr: {stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}
