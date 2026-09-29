//! Regression tests: `silt fmt` must not rewrite an already-formatted
//! file.
//!
//! Pre-fix, `format_file` unconditionally `fs::write`'d the formatted
//! output even when it was byte-identical to the source, bumping the
//! file's mtime on every run. That spuriously retriggered `--watch`
//! loops and mtime-based build tools on every no-op format (e.g. editor
//! format-on-save alongside `silt run --watch`). rustfmt/gofmt skip
//! identical writes for exactly this reason; these tests lock in the
//! same behavior for silt.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_cli_fmt_idempotent_mtime_tests");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{prefix}_{n}.silt"));
    fs::write(&path, content).unwrap();
    path
}

/// Backdate the file's mtime well into the past so that any rewrite
/// (which stamps ~now) is detectable regardless of filesystem
/// timestamp granularity, then return the mtime actually recorded.
fn backdate_mtime(path: &PathBuf) -> SystemTime {
    let backdated = SystemTime::now() - Duration::from_secs(3600);
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(backdated).unwrap();
    drop(file);
    fs::metadata(path).unwrap().modified().unwrap()
}

// ── 1. Already-formatted file: fmt must not touch it ──────────────────

#[test]
fn fmt_skips_write_when_file_already_formatted() {
    // Canonical formatted form (same fixture the exit-code tests use
    // to assert `--check` exits 0).
    let content = "fn main() {\n  println(\"hello\")\n}\n";
    let path = temp_silt_file("already_formatted", content);
    let before = backdate_mtime(&path);

    let output = silt_cmd()
        .arg("fmt")
        .arg(&path)
        .output()
        .expect("failed to run silt");
    assert!(
        output.status.success(),
        "expected exit 0 formatting an already-formatted file, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let after = fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(
        before, after,
        "silt fmt rewrote an already-formatted file: mtime bumped from \
         {before:?} to {after:?} — identical output must skip the write \
         so --watch loops and mtime-based build tools don't retrigger"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        content,
        "file content must be unchanged"
    );
}

// ── 2. Positive control: unformatted file is still rewritten ──────────

#[test]
fn fmt_still_writes_when_file_needs_formatting() {
    // Guard against over-fixing: the skip must apply only to identical
    // output, not suppress real formatting writes.
    let path = temp_silt_file("needs_reformat", "fn  main( ) {\nprintln(\"hello\")\n}\n");
    let before = backdate_mtime(&path);

    let output = silt_cmd()
        .arg("fmt")
        .arg(&path)
        .output()
        .expect("failed to run silt");
    assert!(
        output.status.success(),
        "expected exit 0 formatting a reformattable file, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let formatted = fs::read_to_string(&path).unwrap();
    assert!(
        formatted.contains("fn main()"),
        "expected normalized signature, got:\n{formatted}"
    );
    let after = fs::metadata(&path).unwrap().modified().unwrap();
    assert!(
        after > before,
        "a real reformat must actually write the file (mtime should advance \
         past the backdated {before:?}, got {after:?})"
    );
}
