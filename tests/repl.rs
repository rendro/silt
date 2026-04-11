//! Session-level integration tests for `silt repl`.
//!
//! These tests spawn the built `silt repl` subprocess with piped
//! stdin/stdout/stderr, send a scripted sequence of commands, and assert
//! on the resulting output. They exercise the interactive loop in
//! `src/repl.rs::run_repl` — multi-line accumulation, `:help`/`:quit`,
//! persistent bindings across lines, and error recovery — which is not
//! reachable from the in-process unit tests.
//!
//! Determinism: we write the full script (always ending in `:quit\n`) to
//! stdin, close stdin, and then wait for the child to exit. A watchdog
//! thread enforces a hard timeout so a hung REPL fails the test instead
//! of hanging CI. There are no sleeps or timing-dependent assertions.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Hard upper bound on how long a single REPL session may take.
/// Any real session here finishes in milliseconds; this is only to
/// keep a deadlocked REPL from hanging the test runner.
const SESSION_TIMEOUT: Duration = Duration::from_secs(15);

fn silt_cmd() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_silt"));
    cmd.arg("repl");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd
}

/// Captured output from a scripted REPL session.
struct SessionOutput {
    stdout: String,
    stderr: String,
    success: bool,
}

/// Drive a REPL session: spawn `silt repl`, send `script` to stdin
/// (with `:quit\n` appended unconditionally), close stdin, wait for the
/// child to exit within `SESSION_TIMEOUT`, and return captured output.
///
/// If the child does not exit in time it is killed and the test panics
/// with a clear message — no silent hangs.
fn run_session(script: &str) -> SessionOutput {
    let mut child: Child = silt_cmd()
        .spawn()
        .expect("failed to spawn `silt repl` subprocess");

    // Write the full scripted input, always terminated by `:quit\n` so
    // the REPL exits cleanly regardless of what the caller wrote.
    {
        let stdin = child.stdin.as_mut().expect("child stdin was not piped");
        stdin
            .write_all(script.as_bytes())
            .expect("failed to write script to repl stdin");
        if !script.ends_with('\n') {
            stdin
                .write_all(b"\n")
                .expect("failed to write trailing newline");
        }
        stdin
            .write_all(b":quit\n")
            .expect("failed to write :quit to repl stdin");
    }
    // Drop stdin to signal EOF, in case the REPL ignores `:quit` mid-buffer.
    drop(child.stdin.take());

    // Wait for exit on a helper thread so we can enforce a timeout
    // without busy-polling or sleeping on the main thread.
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let result = child.wait_with_output();
        // Ignore send errors: receiver may have timed out and gone away.
        let _ = tx.send(result);
    });

    match rx.recv_timeout(SESSION_TIMEOUT) {
        Ok(Ok(out)) => {
            // Join the reader thread; it has already produced its value.
            let _ = handle.join();
            SessionOutput {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                success: out.status.success(),
            }
        }
        Ok(Err(e)) => panic!("failed to wait on repl child: {e}"),
        Err(_) => panic!(
            "repl session did not exit within {}s — possible hang in run_repl",
            SESSION_TIMEOUT.as_secs()
        ),
    }
}

/// Every session should start with the banner line. Having this in one
/// place keeps each test focused on the behavior it actually exercises.
fn assert_has_banner(out: &SessionOutput) {
    assert!(
        out.stdout.contains("Silt REPL"),
        "expected REPL banner in stdout, got:\nSTDOUT:\n{}\nSTDERR:\n{}",
        out.stdout,
        out.stderr
    );
}

// ── 1. Simple expression evaluation ─────────────────────────────────

#[test]
fn simple_expression_evaluates_and_prints_result() {
    let out = run_session("1 + 2\n");
    assert_has_banner(&out);
    assert!(
        out.success,
        "repl should exit successfully, stderr: {}",
        out.stderr
    );
    // `1 + 2` is an expression; the REPL prints the value on its own line.
    assert!(
        out.stdout.lines().any(|l| l.trim() == "3"),
        "expected `3` on a line of stdout, got:\n{}",
        out.stdout
    );
}

// ── 2. Multi-line function definition and invocation ───────────────

#[test]
fn multiline_function_definition_then_invocation() {
    // `fn double(x) {` has an unclosed `{`, so the REPL keeps accumulating
    // lines until the matching `}` closes it. After the function is
    // defined, a later input calls it and must see the result.
    let script = "\
fn double(x) {
  x * 2
}
double(21)
";
    let out = run_session(script);
    assert_has_banner(&out);
    assert!(
        out.success,
        "repl should exit successfully, stderr: {}",
        out.stderr
    );
    assert!(
        out.stdout.lines().any(|l| l.trim() == "42"),
        "expected `42` from `double(21)` in stdout, got:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    // Declarations themselves should produce no error output.
    assert!(
        !out.stderr.to_lowercase().contains("error"),
        "unexpected error output from multi-line fn declaration: {}",
        out.stderr
    );
}

// ── 3. `:help` command produces help text ──────────────────────────

#[test]
fn help_command_prints_help_text() {
    let out = run_session(":help\n");
    assert_has_banner(&out);
    assert!(out.success, "repl should exit successfully");
    // `print_help` writes a "Commands:" header and lines describing
    // `:help` and `:quit`. Assert on those literal strings.
    assert!(
        out.stdout.contains("Commands:"),
        "expected `Commands:` header from :help, got:\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains(":help") && out.stdout.contains(":quit"),
        "expected :help and :quit entries in help text, got:\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Multi-line input"),
        "expected multi-line note in help text, got:\n{}",
        out.stdout
    );
}

// ── 3b. `:h` short form is equivalent ──────────────────────────────

#[test]
fn help_short_form_prints_help_text() {
    let out = run_session(":h\n");
    assert!(
        out.stdout.contains("Commands:"),
        "expected :h to print help, got:\n{}",
        out.stdout
    );
}

// ── 4. `:quit` cleanly exits ───────────────────────────────────────

#[test]
fn quit_command_exits_cleanly() {
    // `run_session` already appends `:quit`, but we send it explicitly
    // first here so we're asserting the REPL's own handling — not the
    // harness's safety-net. A clean exit means: status 0, banner shown,
    // and no output on stderr.
    let out = run_session(":quit\n");
    assert_has_banner(&out);
    assert!(
        out.success,
        "`:quit` should cause a successful exit, stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.trim().is_empty(),
        "expected no stderr from a clean :quit, got: {}",
        out.stderr
    );
}

// ── 4b. `:q` short form also exits ─────────────────────────────────

#[test]
fn quit_short_form_exits_cleanly() {
    let out = run_session(":q\n");
    assert!(out.success, "`:q` should cause a successful exit");
}

// ── 5. Error recovery — a bad input does not kill the session ──────

#[test]
fn parse_error_does_not_kill_session() {
    // `1 +` is a parse error (no right operand). The next input must
    // still evaluate in the same process, proving the REPL caught the
    // error and kept its VM / type context intact.
    let script = "\
1 +
2 + 3
";
    let out = run_session(script);
    assert_has_banner(&out);
    assert!(
        out.success,
        "repl should exit successfully despite parse error, stderr: {}",
        out.stderr
    );
    // The parse error is reported on stderr…
    assert!(
        out.stderr.contains("error")
            || out.stderr.contains("expected")
            || out.stderr.contains("parse"),
        "expected a parse-error diagnostic on stderr, got:\n{}",
        out.stderr
    );
    // …and the *next* input still evaluated: `2 + 3 == 5`.
    assert!(
        out.stdout.lines().any(|l| l.trim() == "5"),
        "expected `5` from `2 + 3` after recovery, got stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn type_error_does_not_kill_session() {
    // `1 + "hi"` is a type error. Afterwards, a plain integer literal
    // must still round-trip through the REPL.
    let script = "\
1 + \"hi\"
7
";
    let out = run_session(script);
    assert!(
        out.success,
        "repl should exit successfully despite type error, stderr: {}",
        out.stderr
    );
    // Diagnostic on stderr.
    assert!(
        !out.stderr.trim().is_empty(),
        "expected a diagnostic on stderr for type error, got empty stderr"
    );
    // Follow-up input evaluated.
    assert!(
        out.stdout.lines().any(|l| l.trim() == "7"),
        "expected `7` after type-error recovery, got stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

// ── 6. Let-binding persistence across lines in one session ─────────

#[test]
fn let_binding_persists_across_lines() {
    // `let x = 5` followed by `x + 1` on the *next* line must yield 6,
    // proving that the VM and type context persist between REPL inputs.
    let script = "\
let x = 5
x + 1
";
    let out = run_session(script);
    assert_has_banner(&out);
    assert!(
        out.success,
        "repl should exit successfully, stderr: {}",
        out.stderr
    );
    assert!(
        out.stdout.lines().any(|l| l.trim() == "6"),
        "expected `6` from `x + 1` after `let x = 5`, got stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        !out.stderr.to_lowercase().contains("error"),
        "unexpected error output: {}",
        out.stderr
    );
}

#[test]
fn multiple_let_bindings_compose() {
    // Stacking let bindings proves the persistent context isn't just
    // remembering one name but a growing environment.
    let script = "\
let a = 10
let b = a + 5
let c = b * 2
c
";
    let out = run_session(script);
    assert!(
        out.success,
        "repl should exit successfully, stderr: {}",
        out.stderr
    );
    assert!(
        out.stdout.lines().any(|l| l.trim() == "30"),
        "expected `30` from chained let bindings, got stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
}

// ── 7. Blank lines are ignored, not errors ─────────────────────────

#[test]
fn blank_lines_are_ignored() {
    // The REPL loop `continue`s on empty input when no buffer is
    // accumulating. A session made of blank lines plus one expression
    // should emit exactly one result and no errors.
    let script = "\n\n42\n\n";
    let out = run_session(script);
    assert!(out.success, "repl should exit successfully");
    assert!(
        out.stdout.lines().any(|l| l.trim() == "42"),
        "expected `42` despite surrounding blank lines, got:\n{}",
        out.stdout
    );
    assert!(
        !out.stderr.to_lowercase().contains("error"),
        "blank lines should not produce errors, got: {}",
        out.stderr
    );
}

// ── 8. E2 regression: runtime error shows call stack ──────────────
//
// Previously the REPL's runtime-error path printed only the error site
// via `SourceError` when a span was present, silently dropping the
// `VmError::call_stack` that the VM populates. Users had no way to see
// which function called which when an error happened deep inside a
// declaration-level chain.
//
// Defining `fn g() { 1 / 0 }` and `fn f() { g() }` across two REPL
// turns, then calling `f()`, must surface both `f` and `g` on stderr in
// addition to the division-by-zero error itself.

#[test]
fn test_runtime_error_shows_call_stack() {
    // Note: both call sites deliberately avoid tail position — otherwise
    // the VM's tail-call optimisation collapses the frames and the user
    // would never see `f` in the stack. `g() + 0` and `f() + 0` keep the
    // call frames live so the enriched error carries the full chain.
    let script = "\
fn g() { 1 / 0 }
fn f() { g() + 0 }
f() + 0
";
    let out = run_session(script);
    assert_has_banner(&out);
    assert!(
        out.success,
        "repl should exit successfully despite runtime error, stderr: {}",
        out.stderr
    );
    // The error message itself must be present (division by zero).
    assert!(
        out.stderr.contains("division by zero"),
        "expected `division by zero` in stderr, got:\n{}",
        out.stderr
    );
    // The call stack must name both g (the error site) and f (the caller).
    // The exact formatting is intentionally loose — we want the names to
    // appear on stderr in some frame line.
    assert!(
        out.stderr.contains("-> g"),
        "expected `g` frame in call stack, got stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("-> f"),
        "expected `f` frame in call stack, got stderr:\n{}",
        out.stderr
    );
}
