//! Round-74 BROKEN regression: defining `fn main()` in the REPL must
//! not hang every subsequent expression.
//!
//! Background
//! ----------
//! The REPL's expression-eval path used to wrap user input in
//! `fn main() { <input> }` so the existing compiler entry point
//! (`compile_program` emits `GetGlobal "main"; Call 0; Return`) could
//! drive it. That worked fine for stateless one-liners, but the moment
//! a user defined their own `fn main()` the wrapper redefined it on
//! every later expression. If the user then typed `main()`, the
//! synthetic wrapper's body became `main()` — pointing back at the
//! wrapper itself — and the program self-recursed forever.
//!
//! The fix uses a fresh synthetic name (`__repl_eval_<n>`) per
//! evaluation and a parameterized `compile_program_with_entry` so the
//! wrapper cannot collide with any user-defined top-level function.
//!
//! Runtime, not compile-time
//! -------------------------
//! The original bug is a runtime dispatch bug — the wrapper compiles
//! cleanly and only loops at execution time. A typecheck-only test
//! cannot detect it. This test must therefore actually spawn the REPL
//! subprocess, feed it a `fn main` definition followed by a `main()`
//! invocation, and verify the process exits cleanly within a deadline
//! (rather than being killed by the watchdog).

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Hard upper bound on a session that should finish in milliseconds.
/// If the REPL hangs (e.g. infinite recursion) the watchdog fires and
/// the test fails with a clear message instead of stalling CI.
const SESSION_TIMEOUT: Duration = Duration::from_secs(15);

fn silt_cmd() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_silt"));
    cmd.arg("repl");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd
}

struct SessionOutput {
    stdout: String,
    stderr: String,
    success: bool,
}

fn run_session(script: &str) -> SessionOutput {
    let mut child: Child = silt_cmd()
        .spawn()
        .expect("failed to spawn `silt repl` subprocess");

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
    drop(child.stdin.take());

    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });

    match rx.recv_timeout(SESSION_TIMEOUT) {
        Ok(Ok(out)) => {
            let _ = handle.join();
            SessionOutput {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                success: out.status.success(),
            }
        }
        Ok(Err(e)) => panic!("failed to wait on repl child: {e}"),
        Err(_) => panic!(
            "REPL session did not exit within {}s — round-74 BROKEN regression \
             reproduced: a user-defined `fn main()` in the REPL is hanging the \
             evaluator (likely infinite self-recursion in the synthetic wrapper)",
            SESSION_TIMEOUT.as_secs()
        ),
    }
}

/// Core regression: declare `fn main()` and then call it. Pre-fix this
/// would hang because the second input got wrapped in a fresh
/// `fn main() { main() }`, which self-recursed. Post-fix the wrapper
/// uses a unique synthetic name (`__repl_eval_<n>`), so calling
/// `main()` resolves to the user's original definition and returns
/// normally.
#[test]
fn fn_main_then_call_does_not_hang() {
    let script = "\
fn main() { 42 }
main()
";
    let out = run_session(script);
    assert!(
        out.success,
        "REPL should exit successfully after `fn main()` + `main()`. stderr:\n{}\nstdout:\n{}",
        out.stderr, out.stdout,
    );
    // `main()` returns 42; the REPL prints non-Unit expression results.
    assert!(
        out.stdout.lines().any(|l| l.trim() == "42"),
        "expected `42` from `main()` invocation in stdout, got:\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr,
    );
}

/// Stress: redefining `fn main()` and then evaluating a *different*
/// expression that doesn't even mention `main` must also work — the
/// pre-fix bug applied to every subsequent expression because the
/// wrapper for the second expression unconditionally redefined `main`,
/// shadowing the user's binding so a later `main()` (from the user)
/// would still self-recurse. We assert two unrelated expressions work
/// after the redefinition.
#[test]
fn fn_main_then_unrelated_expressions_do_not_hang() {
    let script = "\
fn main() { 7 }
1 + 2
main()
3 * 4
";
    let out = run_session(script);
    assert!(
        out.success,
        "REPL should exit successfully after redefining main and evaluating multiple expressions. stderr:\n{}\nstdout:\n{}",
        out.stderr, out.stdout,
    );
    // Each expression's printed value should appear on its own line.
    let stdout_lines: Vec<&str> = out.stdout.lines().map(|l| l.trim()).collect();
    assert!(
        stdout_lines.iter().any(|l| *l == "3"),
        "expected `3` from `1 + 2`, got:\n{}",
        out.stdout
    );
    assert!(
        stdout_lines.iter().any(|l| *l == "7"),
        "expected `7` from `main()`, got:\n{}",
        out.stdout
    );
    assert!(
        stdout_lines.iter().any(|l| *l == "12"),
        "expected `12` from `3 * 4`, got:\n{}",
        out.stdout
    );
}
