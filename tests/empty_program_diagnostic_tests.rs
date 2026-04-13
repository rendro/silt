//! Round-24 B-fix lock: the missing-main diagnostic must render with
//! the canonical `error[compile]:` header shape (same as every other
//! compile-phase diagnostic), not as a bare `{path}: message` line.
//!
//! Both `silt run` and `silt check` share the diagnostic: an empty
//! program (or any program with no `main` function) exits 1 with
//! `error[compile]: program has no main() function` and the usual
//! rustc-style note body.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_empty_program_diagnostic_tests");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

// ── silt run ─────────────────────────────────────────────────────────

#[test]
fn test_silt_run_empty_file_emits_error_compile_header() {
    let path = temp_silt_file("run_empty", "");

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success(), "expected non-zero exit");
    assert_eq!(output.status.code(), Some(1));

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Canonical header shape (same as parse/type/runtime errors).
    assert!(
        stderr.contains("error[compile]:"),
        "missing `error[compile]:` header, got:\n{stderr}"
    );
    // Still contains the human-readable payload.
    assert!(
        stderr.contains("program has no main() function"),
        "missing payload message, got:\n{stderr}"
    );
    // Must NOT be the old plain `{path}: program has no main()` shape —
    // i.e. the line containing the message must start with `error[`,
    // not the file path.
    let diag_line = stderr
        .lines()
        .find(|l| l.contains("program has no main() function"))
        .expect("expected a line carrying the main() message");
    assert!(
        diag_line.contains("error[compile]"),
        "expected canonical header on the diagnostic line, got: {diag_line:?}"
    );
    // No Rust panics.
    assert!(
        !stderr.contains("panicked at"),
        "stderr should not contain a Rust panic, got: {stderr}"
    );
}

#[test]
fn test_silt_run_no_main_function_emits_error_compile_header() {
    // A non-empty program that still has no `main` — exercises the
    // same missing-main path with a non-trivial source text.
    let path = temp_silt_file("run_no_main", "fn helper() { 42 }\n");

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error[compile]:"),
        "missing `error[compile]:` header, got:\n{stderr}"
    );
    assert!(
        stderr.contains("program has no main() function"),
        "missing payload message, got:\n{stderr}"
    );
}

#[test]
fn test_silt_run_test_file_suggests_silt_test_with_error_compile_header() {
    // A file that looks like a test file (fn test_...) — the special
    // "silt test" nudge still goes through the canonical header.
    // No `test.` calls here; looks_like_test_file keys on `fn test_`
    // alone for the detector, and we want to avoid introducing a
    // separate "module 'test' not imported" diagnostic that would
    // take precedence over the missing-main check.
    let path = temp_silt_file(
        "run_test_file",
        "fn test_example() {\n  let x = 1\n  x + 1\n}\n",
    );

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error[compile]:"),
        "missing canonical header, got:\n{stderr}"
    );
    assert!(
        stderr.contains("program has no main() function"),
        "missing primary message, got:\n{stderr}"
    );
    assert!(
        stderr.contains("silt test"),
        "missing `silt test` nudge, got:\n{stderr}"
    );
}

// ── silt check ───────────────────────────────────────────────────────

#[test]
fn test_silt_check_empty_file_emits_error_compile_header() {
    let path = temp_silt_file("check_empty", "");

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    // `silt check` must surface the same missing-main diagnostic as
    // `silt run` — otherwise the file passes `check` cleanly and then
    // fails at `run`, which is off-spec.
    assert!(
        !output.status.success(),
        "expected non-zero exit for empty file under `silt check`, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(1));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error[compile]:"),
        "missing canonical header, got:\n{stderr}"
    );
    assert!(
        stderr.contains("program has no main() function"),
        "missing payload, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "stderr should not contain a Rust panic, got: {stderr}"
    );
}

#[test]
fn test_silt_check_no_main_function_emits_error_compile_header() {
    // Non-empty program with no main function. Same shape as above.
    let path = temp_silt_file("check_no_main", "fn helper() { 42 }\n");

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error[compile]:"),
        "missing canonical header, got:\n{stderr}"
    );
    assert!(
        stderr.contains("program has no main() function"),
        "missing payload, got:\n{stderr}"
    );
}

#[test]
fn test_silt_check_with_main_function_is_clean() {
    // Regression guard: once a program has a main, `silt check` must
    // exit 0 — the missing-main detector must not over-trigger.
    let path = temp_silt_file("check_has_main", "fn main() { println(\"hi\") }\n");

    let output = silt_cmd()
        .arg("check")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected exit 0 for program with main(), stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_silt_run_with_main_function_does_not_show_missing_main_error() {
    // Regression guard on the `run` path: a valid program must not
    // spuriously report a missing-main error.
    let path = temp_silt_file(
        "run_has_main",
        "fn main() { println(\"ok\") }\n",
    );

    let output = silt_cmd()
        .arg("run")
        .arg(&path)
        .output()
        .expect("failed to run silt");

    assert!(
        output.status.success(),
        "expected success for valid main-having program, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("program has no main() function"),
        "should not surface missing-main on valid program, got: {stderr}"
    );
}
