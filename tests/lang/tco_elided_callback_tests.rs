//! Regression tests for L3: `invoke_callable` must prune `tco_elided`
//! on successful callback return.
//!
//! When a callback passed to a higher-order builtin (list.map, list.filter,
//! list.fold, etc.) uses tail calls, the `tco_elided` diagnostic entries
//! from successful iterations must be pruned before the next iteration.
//! Otherwise, if a later iteration errors, `enrich_error` includes stale
//! entries from prior successful iterations, producing phantom frames in
//! the rendered call stack.
//!
//! Fix: add `self.prune_tco_elided(self.frames.len())` after frame pops
//! in `invoke_callable`'s Return and EarlyReturn arms, mirroring what
//! `execute()` and `execute_slice()` already do.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Create a unique temporary .silt file.
fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_tco_elided_callback");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

/// Run `silt run <path>` and capture stderr.
fn run_silt_stderr(path: &std::path::Path) -> String {
    let output = silt_cmd()
        .arg("run")
        .arg(path)
        .output()
        .expect("failed to run silt binary");
    assert!(
        !output.status.success(),
        "expected non-zero exit for error case, got success. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Run `silt run <path>` and capture stdout (for success cases).
fn run_silt_stdout(path: &std::path::Path) -> String {
    let output = silt_cmd()
        .arg("run")
        .arg(path)
        .output()
        .expect("failed to run silt binary");
    assert!(
        output.status.success(),
        "expected zero exit, got failure. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// ── Test 1: No phantom frames from prior iterations ────────────────

/// When a callback with a tail call errors on a later iteration,
/// the call stack must NOT include phantom `wrapper` frames from
/// prior successful iterations.
///
/// Reproduction from the finding (adapted to silt syntax):
///   fn helper(x) { match x == 0 { true -> 1 / 0, false -> x } }
///   fn wrapper(x) { helper(x) }    // tail call to helper
///   fn main() { list.map([1, 2, 0], wrapper) }
///
/// Before the fix, `wrapper` appeared multiple times in the call stack
/// (once per successful iteration). After the fix, it appears exactly once.
#[test]
fn test_tco_elided_callback_no_phantom_frames() {
    let path = temp_silt_file(
        "tco_phantom",
        r#"import list
fn helper(x) {
  match x == 0 {
    true -> 1 / 0,
    false -> x
  }
}
fn wrapper(x) { helper(x) }
fn main() { list.map([1, 2, 0], wrapper) }
"#,
    );

    let stderr = run_silt_stderr(&path);

    // (a) Must contain the division-by-zero error.
    assert!(
        stderr.contains("division by zero"),
        "expected 'division by zero' in stderr, got:\n{stderr}"
    );

    // (b) `wrapper` must appear in the call stack, but only ONCE.
    // Before the fix, stale tco_elided entries caused `wrapper` to
    // appear once for every prior successful callback iteration.
    let wrapper_count = stderr.matches("-> wrapper").count();
    assert!(
        wrapper_count == 1,
        "expected exactly 1 '-> wrapper' in call stack, got {wrapper_count}. stderr:\n{stderr}"
    );

    // (c) `helper` must appear in the call stack exactly once.
    let helper_count = stderr.matches("-> helper").count();
    assert!(
        helper_count == 1,
        "expected exactly 1 '-> helper' in call stack, got {helper_count}. stderr:\n{stderr}"
    );

    // (d) The error snippet must show the callback body.
    assert!(
        stderr.contains("1 / 0"),
        "expected '1 / 0' in error snippet, got:\n{stderr}"
    );
}

// ── Test 2: Successful callbacks with tail calls still work ────────

/// Callbacks that use tail calls but never error must still produce
/// correct results. This ensures `prune_tco_elided` doesn't break
/// the normal return path.
#[test]
fn test_tco_elided_callback_successful_still_works() {
    let path = temp_silt_file(
        "tco_success",
        r#"import list
fn double_helper(x) { x * 2 }
fn double_wrapper(x) { double_helper(x) }
fn main() {
    let mapped = list.map([1, 2, 3, 4, 5], double_wrapper)
    print(mapped)
}
"#,
    );

    let stdout = run_silt_stdout(&path);

    // The output must contain the correctly mapped list.
    assert!(
        stdout.contains("[2, 4, 6, 8, 10]"),
        "expected '[2, 4, 6, 8, 10]' in stdout, got:\n{stdout}"
    );
}
