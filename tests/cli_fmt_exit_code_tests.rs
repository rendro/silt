//! Regression tests for the `silt fmt --check` exit-code taxonomy.
//!
//! Audit round 50 split `silt fmt --check`'s exit codes into three
//! distinct signals so CI callers can distinguish drift from infra
//! failure:
//!
//!   - exit 0 — every input file is already formatted.
//!   - exit 1 — at least one file would be reformatted (the intended
//!              `--check` signal).
//!   - exit 2 — at least one file failed to read or parse (infra
//!              failure — the check is inconclusive).
//!
//! Previously all three conditions collapsed to exit 1, which meant a
//! parse-broken file produced the same signal as a well-formed file
//! that merely needed formatting. These tests lock in the split.

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
    let dir = std::env::temp_dir().join("silt_cli_fmt_exit_code_tests");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

fn exit_code(cmd: &mut Command) -> (i32, String, String) {
    let output = cmd.output().expect("failed to run silt");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (code, stdout, stderr)
}

// ── 1. Already-formatted file → exit 0 ────────────────────────────────

#[test]
fn fmt_check_formatted_file_exits_zero() {
    let path = temp_silt_file(
        "already_formatted",
        "fn main() {\n  println(\"hello\")\n}\n",
    );

    let (code, _stdout, stderr) = exit_code(silt_cmd().arg("fmt").arg("--check").arg(&path));

    assert_eq!(
        code, 0,
        "expected exit 0 for a well-formatted file, got {code}, stderr:\n{stderr}"
    );
}

// ── 2. Reformattable file → exit 1 ────────────────────────────────────

#[test]
fn fmt_check_reformattable_file_exits_one() {
    // Extra whitespace in signature + unindented body: parses fine but
    // the formatter would rewrite it.
    let path = temp_silt_file("needs_reformat", "fn  main( ) {\nprintln(\"hello\")\n}\n");

    let (code, _stdout, stderr) = exit_code(silt_cmd().arg("fmt").arg("--check").arg(&path));

    assert_eq!(
        code, 1,
        "expected exit 1 for a reformattable file, got {code}, stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("not formatted"),
        "expected 'not formatted' diagnostic, got stderr:\n{stderr}"
    );
}

// ── 3. Parse-error file → exit 2 ──────────────────────────────────────

#[test]
fn fmt_check_unparseable_file_exits_two() {
    // Deliberate syntax error: unmatched paren in fn signature.
    let path = temp_silt_file("parse_error", "fn broken( {\n}\n");

    let (code, _stdout, stderr) = exit_code(silt_cmd().arg("fmt").arg("--check").arg(&path));

    assert_eq!(
        code, 2,
        "expected exit 2 for an unparseable file, got {code}, stderr:\n{stderr}"
    );
    // The formatter's lex/parse error is rendered as a SourceError, so
    // the `-->` locator line should appear.
    assert!(
        stderr.contains("-->"),
        "expected '-->' locator in diagnostic, got stderr:\n{stderr}"
    );
}

// ── 4. Non-existent file → exit 2 ─────────────────────────────────────

#[test]
fn fmt_check_nonexistent_file_exits_two() {
    // A path that definitely doesn't exist. We still point at the
    // temp-dir scratch space so the error message can only come from
    // the I/O read, not from some parent-lookup scanner.
    let dir = std::env::temp_dir().join("silt_cli_fmt_exit_code_tests");
    fs::create_dir_all(&dir).unwrap();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let missing = dir.join(format!("definitely_not_here_{n}.silt"));
    assert!(
        !missing.exists(),
        "precondition: expected missing path to not exist: {}",
        missing.display()
    );

    let (code, _stdout, stderr) = exit_code(silt_cmd().arg("fmt").arg("--check").arg(&missing));

    assert_eq!(
        code, 2,
        "expected exit 2 for a non-existent file, got {code}, stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("error reading"),
        "expected 'error reading' diagnostic for missing file, got stderr:\n{stderr}"
    );
}

// ── 5. Mixed: drift + infra error → exit 2 (infra dominates) ──────────
//
// If a run contains both drift and infra failure, we must surface the
// infra failure (exit 2) — otherwise CI would treat the inconclusive
// state as plain drift and potentially auto-fix the broken file.

#[test]
fn fmt_check_mixed_drift_and_infra_error_exits_two() {
    let drift_path = temp_silt_file("mixed_drift", "fn  main( ) {\nprintln(\"hi\")\n}\n");
    let parse_path = temp_silt_file("mixed_parse", "fn broken( {\n}\n");

    let (code, _stdout, stderr) = exit_code(
        silt_cmd()
            .arg("fmt")
            .arg("--check")
            .arg(&drift_path)
            .arg(&parse_path),
    );

    assert_eq!(
        code, 2,
        "expected exit 2 when infra error coexists with drift, got {code}, stderr:\n{stderr}"
    );
}
