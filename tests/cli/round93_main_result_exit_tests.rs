//! Round-93 regression tests: `Err` escaping `main` must fail the
//! process.
//!
//! Audit round 93 gap: `silt run` discarded `vm.run`'s Ok value (main's
//! return value) wholesale, so `fn main() -> Result(Int, String) {
//! Err("boom") }` — or a `?` propagating an Err out of main — exited 0
//! with no diagnostic. CI/shell callers saw success on failure, while
//! `silt test` correctly exited 1 on failing tests.
//!
//! The fix (src/cli/run.rs::vm_run_file) inspects main's return value:
//! an `Err(..)` variant renders a canonical `error[runtime]: main
//! returned Err: <payload>` diagnostic to stderr and exits 1 (the same
//! code every other runtime error uses). `Ok(..)` returns and
//! non-Result returns (Unit, Int, ...) keep the old exit-0 behavior.
//!
//! Doc: docs/language/error-handling.md "`Err` escaping `main`".

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
    let dir = std::env::temp_dir().join(format!(
        "silt_round93_main_result_exit_tests_{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{prefix}_{n}.silt"));
    fs::write(&path, content).unwrap();
    path
}

/// Run `silt run <file>` and return (exit code, stdout, stderr).
fn run_file(path: &PathBuf) -> (i32, String, String) {
    let output = silt_cmd()
        .arg("run")
        .arg(path)
        .output()
        .expect("failed to spawn silt");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (code, stdout, stderr)
}

// ── 1. Direct Err from main → exit 1 + diagnostic ────────────────────

#[test]
fn err_from_main_exits_nonzero_with_payload_on_stderr() {
    let path = temp_silt_file(
        "direct_err",
        "fn main() -> Result(Int, String) {\n  Err(\"boom\")\n}\n",
    );
    let (code, _stdout, stderr) = run_file(&path);
    assert_eq!(
        code, 1,
        "Err escaping main must exit 1 (the runtime-error exit code), got {code}; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("boom"),
        "stderr must mention the Err payload; got:\n{stderr}"
    );
    assert!(
        stderr.contains("error[runtime]"),
        "diagnostic must use the canonical error[runtime] header; got:\n{stderr}"
    );
    assert!(
        stderr.contains("main returned Err"),
        "diagnostic must say main returned Err; got:\n{stderr}"
    );
}

// ── 2. `?` propagating an Err out of main → exit 1 ───────────────────

#[test]
fn question_mark_propagating_err_out_of_main_exits_nonzero() {
    let path = temp_silt_file(
        "propagated_err",
        "fn fail() -> Result(Int, String) {\n  Err(\"kaput\")\n}\n\nfn main() -> Result(Int, String) {\n  let v = fail()?\n  Ok(v)\n}\n",
    );
    let (code, _stdout, stderr) = run_file(&path);
    assert_eq!(
        code, 1,
        "`?` propagating Err out of main must exit 1, got {code}; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("kaput"),
        "stderr must mention the propagated payload; got:\n{stderr}"
    );
}

// ── 3. Ok from main → exit 0, no error output ────────────────────────

#[test]
fn ok_from_main_exits_zero_with_no_error_output() {
    let path = temp_silt_file(
        "ok_main",
        "fn main() -> Result(Int, String) {\n  Ok(7)\n}\n",
    );
    let (code, _stdout, stderr) = run_file(&path);
    assert_eq!(
        code, 0,
        "Ok from main must exit 0, got {code}; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("error["),
        "Ok from main must not print any diagnostic; stderr:\n{stderr}"
    );
}

// ── 4. Plain Unit main → unchanged ───────────────────────────────────

#[test]
fn plain_unit_main_unchanged() {
    let path = temp_silt_file("plain_main", "fn main() {\n  println(\"hi\")\n}\n");
    let (code, stdout, stderr) = run_file(&path);
    assert_eq!(
        code, 0,
        "plain main must exit 0, got {code}; stderr:\n{stderr}"
    );
    assert_eq!(stdout, "hi\n", "stdout must be exactly the println output");
    assert!(
        !stderr.contains("error["),
        "plain main must not print any diagnostic; stderr:\n{stderr}"
    );
}

// ── 5. Non-Result return value → unchanged ───────────────────────────

#[test]
fn non_result_main_return_value_exits_zero() {
    let path = temp_silt_file("int_main", "fn main() -> Int {\n  42\n}\n");
    let (code, _stdout, stderr) = run_file(&path);
    assert_eq!(
        code, 0,
        "main returning a non-Result value must keep exiting 0, got {code}; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("error["),
        "non-Result main must not print any diagnostic; stderr:\n{stderr}"
    );
}
