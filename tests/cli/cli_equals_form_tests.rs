//! Regression tests: GNU-style `--flag=value` form must be accepted by
//! `silt check` and `silt test`, matching `silt add --path=...` which has
//! supported it since the command was introduced.
//!
//! Before this fix, `src/cli/check.rs` and `src/cli/test.rs` only matched
//! the exact flag string (`--format`, `--filter`), so `--format=json` and
//! `--filter=pat` were rejected as unknown flags. This is an inconsistency
//! across subcommands — `--foo=bar` is the standard GNU convention and
//! should work everywhere.
//!
//! Each test shells out to the built `silt` binary so the full CLI
//! dispatch path is covered, and pins exact substrings in the output so
//! a regression can't hide behind a `.is_ok()` assertion.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Write a throwaway `.silt` file under the system temp dir, unique per
/// invocation so parallel test threads don't collide.
fn temp_silt_file(tag: &str, contents: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("silt_equals_form_{tag}_{pid}_{n}.silt"));
    fs::write(&path, contents).expect("failed to write temp silt file");
    path
}

// ── silt check --format=json ─────────────────────────────────────────────

/// The headline regression: `silt check --format=json <file>` must parse
/// the flag, succeed on a clean file, and emit a JSON array on stdout.
/// This is the exact invocation that was rejected as "unknown flag"
/// before the fix.
#[test]
fn silt_check_accepts_format_equals_json() {
    // A clean file — `silt check` should exit 0 and emit `[]` (an empty
    // JSON array of diagnostics) on stdout.
    let path = temp_silt_file("check_equals_clean", "fn main() {}\n");

    let output = silt_cmd()
        .arg("check")
        .arg("--format=json")
        .arg(&path)
        .output()
        .expect("failed to run silt check --format=json");

    assert!(
        output.status.success(),
        "silt check --format=json exited non-zero on a clean file; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unknown flag"),
        "stderr should not mention 'unknown flag'; got: {stderr}"
    );
    assert!(
        !stderr.contains("--format=json"),
        "stderr should not echo the equals-form flag back as an error; got: {stderr}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Pin the JSON-emission fact: on a clean file the diagnostics array
    // is empty, so stdout must be exactly `[]` plus a trailing newline.
    // This is stronger than `serde_json::from_str(...).is_ok()` — it
    // locks the exact wire shape `--format json` has always produced.
    let trimmed = stdout.trim_end();
    assert_eq!(
        trimmed, "[]",
        "expected empty JSON array on clean file, got stdout: {stdout:?}"
    );

    let _ = fs::remove_file(&path);
}

/// Parallel check: the space-separated form (`--format json`) must still
/// work. We want the `=` form to be an addition, not a replacement.
#[test]
fn silt_check_still_accepts_format_space_json() {
    let path = temp_silt_file("check_space_clean", "fn main() {}\n");

    let output = silt_cmd()
        .arg("check")
        .arg("--format")
        .arg("json")
        .arg(&path)
        .output()
        .expect("failed to run silt check --format json");

    assert!(
        output.status.success(),
        "silt check --format json exited non-zero on a clean file; stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim_end();
    assert_eq!(
        trimmed, "[]",
        "expected empty JSON array on clean file, got stdout: {stdout:?}"
    );

    let _ = fs::remove_file(&path);
}

/// Equals-form with a bad value (`--format=xml`) must produce the same
/// diagnostic as the space-form `--format xml` would: "--format requires
/// 'json'" and a non-zero exit. Pinning this guards against a future
/// "accept anything after `=`" regression.
#[test]
fn silt_check_format_equals_bad_value_rejected() {
    let path = temp_silt_file("check_equals_bad", "fn main() {}\n");

    let output = silt_cmd()
        .arg("check")
        .arg("--format=xml")
        .arg(&path)
        .output()
        .expect("failed to run silt check --format=xml");

    assert!(
        !output.status.success(),
        "silt check --format=xml should exit non-zero; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--format requires 'json'"),
        "stderr should complain about the bad format value; got: {stderr}"
    );

    let _ = fs::remove_file(&path);
}

// ── silt test --filter=pat ───────────────────────────────────────────────

/// The headline regression for `silt test`: `--filter=pat` must be
/// accepted, the substring-match behavior must still kick in, and the
/// tests must actually run.
///
/// Fixture: two test functions, `test_alpha` and `test_beta`. The filter
/// `alpha` should match `test_alpha` but not `test_beta`. We pin:
///   (1) exit status success (one test passes, none fail),
///   (2) the `PASS ...::test_alpha` line appears,
///   (3) the filtered-out `test_beta` does NOT appear in output,
///   (4) no "unknown flag" complaint.
#[test]
fn silt_test_accepts_filter_equals_pattern() {
    // `*_test.silt` so auto-discovery would find it too, though here we
    // invoke with an explicit path for reproducibility.
    let path = temp_silt_file("filter_equals", "fn test_alpha() {}\nfn test_beta() {}\n");
    // Rename extension so the path ends with `_test.silt` — keeps
    // discovery-layer behavior consistent with the filter's contract.
    let renamed = path.with_file_name(format!(
        "{}_test.silt",
        path.file_stem().unwrap().to_string_lossy()
    ));
    fs::rename(&path, &renamed).expect("rename temp file");

    let output = silt_cmd()
        .arg("test")
        .arg("--filter=alpha")
        .arg(&renamed)
        .output()
        .expect("failed to run silt test --filter=alpha");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "silt test --filter=alpha should succeed; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stderr.contains("unknown flag"),
        "stderr should not mention 'unknown flag'; got: {stderr}"
    );
    // `silt test` writes PASS/FAIL lines and the final summary to stderr.
    assert!(
        stderr.contains("PASS") && stderr.contains("test_alpha"),
        "expected 'PASS' + 'test_alpha' in test output; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stderr.contains("test_beta"),
        "test_beta should have been filtered out; stderr={stderr}"
    );
    // The summary line pins the count: exactly 1 test ran, 1 passed.
    assert!(
        stderr.contains("1 test: 1 passed"),
        "expected '1 test: 1 passed' summary; stderr={stderr}"
    );

    let _ = fs::remove_file(&renamed);
}

/// Parallel check: the space-separated form (`--filter pat`) must still
/// work. Same fixture, same assertions, different flag shape.
#[test]
fn silt_test_still_accepts_filter_space_pattern() {
    let path = temp_silt_file("filter_space", "fn test_alpha() {}\nfn test_beta() {}\n");
    let renamed = path.with_file_name(format!(
        "{}_test.silt",
        path.file_stem().unwrap().to_string_lossy()
    ));
    fs::rename(&path, &renamed).expect("rename temp file");

    let output = silt_cmd()
        .arg("test")
        .arg("--filter")
        .arg("alpha")
        .arg(&renamed)
        .output()
        .expect("failed to run silt test --filter alpha");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "silt test --filter alpha should succeed; stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("PASS") && stderr.contains("test_alpha"),
        "expected 'PASS' + 'test_alpha' in test output; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stderr.contains("test_beta"),
        "test_beta should have been filtered out; stderr={stderr}"
    );
    assert!(
        stderr.contains("1 test: 1 passed"),
        "expected '1 test: 1 passed' summary; stderr={stderr}"
    );

    let _ = fs::remove_file(&renamed);
}

/// Equals-form with an empty value (`--filter=`) must fail the same way
/// the space-form with a missing value does: "--filter requires a
/// pattern" and a non-zero exit. Pinning this guards against "silently
/// accept empty pattern and run every test" regressions.
#[test]
fn silt_test_filter_equals_empty_rejected() {
    let path = temp_silt_file("filter_empty", "fn test_alpha() {}\n");
    let renamed = path.with_file_name(format!(
        "{}_test.silt",
        path.file_stem().unwrap().to_string_lossy()
    ));
    fs::rename(&path, &renamed).expect("rename temp file");

    let output = silt_cmd()
        .arg("test")
        .arg("--filter=")
        .arg(&renamed)
        .output()
        .expect("failed to run silt test --filter=");

    assert!(
        !output.status.success(),
        "silt test --filter= should exit non-zero; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--filter requires a pattern"),
        "stderr should complain about the empty filter value; got: {stderr}"
    );

    let _ = fs::remove_file(&renamed);
}
