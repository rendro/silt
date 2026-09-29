//! Session-level integration tests for `silt repl`.
//!
//! These tests spawn the built `silt repl` subprocess with piped
//! stdin/stdout/stderr, send a scripted sequence of commands, and assert
//! on the resulting output. They exercise the interactive loop in
//! `src/repl.rs::run_repl` — multi-line accumulation, `:help`/`:quit`,
//! persistent bindings across lines, and error recovery — which is not
//! reachable from the in-process unit tests.
//!
//! Single-script sessions are golden cases under
//! `tests/golden/cli/repl/` (`-- cmd: repl` with `-- stdin:`); the test
//! left here covers an anonymous-fn form that stage 4 removes.
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

// ── 11. BROKEN: `fn (` anon-fn expressions must not be routed to the
//        declaration parser ───────────────────────────────────────────
//
// Regression lock (round-101): `is_declaration` used the
// whitespace-sensitive heuristic `starts_with("fn ")`, so a valid
// anonymous-fn expression written with a space — `fn (x) { x * 2 }(5)`
// — was routed to `eval_declaration` and rejected with
// `expected identifier, found (`, even though the identical input runs
// fine in file mode (`let y = fn (x) { x * 2 }(5)` prints 10 via
// `silt run`). The parser is token-based and distinguishes `fn IDENT`
// (declaration) from `fn (` (anon-fn expression) regardless of
// whitespace (`parser::at_top_level_fn_start`); `is_declaration` now
// mirrors that rule.

#[test]
fn test_repl_anon_fn_with_space_is_evaluated_as_expression() {
    // Immediately-invoked anonymous fn, with a space after `fn`. Before
    // the fix this printed `expected identifier, found (` on stderr and
    // nothing on stdout.
    let out = run_session("fn (x) { x * 2 }(5)\n");
    assert_has_banner(&out);
    assert!(
        out.success,
        "repl should exit successfully, stderr: {}",
        out.stderr
    );
    assert!(
        out.stdout.lines().any(|l| l.trim() == "10"),
        "expected `10` from `fn (x) {{ x * 2 }}(5)` in stdout, got:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        !out.stderr.contains("expected identifier"),
        "anon-fn expression must not hit the declaration parser, got stderr:\n{}",
        out.stderr
    );

    // A named declaration with the same leading keyword must still take
    // the declaration path and stay callable afterwards.
    let out = run_session("fn triple(x) { x * 3 }\ntriple(4)\n");
    assert!(
        out.stdout.lines().any(|l| l.trim() == "12"),
        "expected `12` from `triple(4)` in stdout, got:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        !out.stderr.to_lowercase().contains("error"),
        "named fn declaration must still route through eval_declaration, got stderr:\n{}",
        out.stderr
    );
}
