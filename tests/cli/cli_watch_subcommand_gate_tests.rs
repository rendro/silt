//! Round-67 audit regression: `--watch` must reject non-runnable
//! invocations up front rather than entering an infinite watch loop.
//!
//! Before the fix, the following invocations would all enter the
//! watcher and silently sit there waiting for file changes:
//!
//!   - `silt --watch`           (no subcommand at all)
//!   - `silt repl --watch`      (interactive — rerun makes no sense)
//!   - `silt update --watch`    (one-shot lockfile regeneration)
//!   - `silt init --watch`      (one-shot scaffold creation)
//!   - `silt lsp --watch`       (long-running stdio server)
//!   - `silt add --watch`       (one-shot manifest mutation)
//!   - `silt self-update --watch` (one-shot binary upgrade)
//!   - `silt fmt --watch`       (rewrite-in-place; rerun = thrash)
//!   - `silt asdfasdf --watch`  (unknown subcommand)
//!
//! Only `run`, `check`, `disasm`, and `test` have meaningful
//! re-execute-on-save semantics.
//!
//! This test does NOT require the `watch` feature to be enabled at
//! build time — without it, the dispatcher rejects `--watch` with a
//! different error message and exits 1, which still satisfies the
//! "didn't enter the loop" guarantee.

use std::process::Command;
use std::time::Duration;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Run `silt` with the given args, kill it after `timeout` if it
/// hasn't exited (which would indicate the watch loop entered), and
/// return (exit-code-or-None, stdout, stderr, timed_out).
fn run_with_timeout(args: &[&str], timeout: Duration) -> (Option<i32>, String, String, bool) {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = silt_cmd()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn silt");

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut stdout);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut stderr);
                }
                return (status.code(), stdout, stderr, false);
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let mut stdout = String::new();
                    let mut stderr = String::new();
                    if let Some(mut s) = child.stdout.take() {
                        let _ = s.read_to_string(&mut stdout);
                    }
                    if let Some(mut s) = child.stderr.take() {
                        let _ = s.read_to_string(&mut stderr);
                    }
                    return (None, stdout, stderr, true);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("try_wait failed: {e}"),
        }
    }
}

/// Helper: assert the gate fired — process exited non-zero quickly,
/// did NOT print the watcher's `[watch]` banner, and stderr mentions
/// the gate's error message (or, if watch feature is off, that
/// rejection's message).
fn assert_gate_rejection(args: &[&str]) {
    let (code, stdout, stderr, timed_out) = run_with_timeout(args, Duration::from_secs(5));

    assert!(
        !timed_out,
        "args {args:?} entered the watch loop (process did not exit within 5s).\nstdout={stdout}\nstderr={stderr}"
    );
    assert_eq!(
        code,
        Some(1),
        "args {args:?} must exit 1 from the gate, got code={code:?}\nstdout={stdout}\nstderr={stderr}"
    );
    // The `[watch] Watching for changes...` banner is the unmistakable
    // sign the loop was entered. The gate must fire BEFORE that prints.
    assert!(
        !stdout.contains("[watch]") && !stderr.contains("[watch]"),
        "args {args:?} entered the watch loop (`[watch]` banner appeared).\nstdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn watch_alone_with_no_subcommand_is_rejected() {
    // `silt --watch` (no subcommand at all). Pre-fix this entered
    // the watcher with an empty argv, reran nothing, and looped.
    assert_gate_rejection(&["--watch"]);
}

#[test]
fn watch_alone_short_form_is_rejected() {
    // `silt -w` — short form of the same bug.
    assert_gate_rejection(&["-w"]);
}

#[test]
fn watch_with_repl_is_rejected() {
    assert_gate_rejection(&["repl", "--watch"]);
}

#[test]
fn watch_with_init_is_rejected() {
    assert_gate_rejection(&["init", "--watch"]);
}

#[test]
fn watch_with_lsp_is_rejected() {
    assert_gate_rejection(&["lsp", "--watch"]);
}

#[test]
fn watch_with_update_is_rejected() {
    assert_gate_rejection(&["update", "--watch"]);
}

#[test]
fn watch_with_add_is_rejected() {
    assert_gate_rejection(&["add", "--watch", "foo", "--path", "../foo"]);
}

#[test]
fn watch_with_self_update_is_rejected() {
    assert_gate_rejection(&["self-update", "--watch"]);
}

#[test]
fn watch_with_fmt_is_rejected() {
    // `silt fmt --watch` would rewrite files in place on every save,
    // which is a feedback loop. Rerunning fmt makes no sense.
    assert_gate_rejection(&["fmt", "--watch"]);
}

#[test]
fn watch_with_unknown_subcommand_is_rejected() {
    // `silt asdfasdf --watch` — subcommand isn't on any allowlist
    // and isn't a known silt subcommand. Reject up front.
    assert_gate_rejection(&["asdfasdf", "--watch"]);
}

#[test]
fn watch_gate_error_names_runnable_subcommands() {
    // The error message must list the subcommands that DO support
    // `--watch` so the user knows the right path forward.
    let (_code, _stdout, stderr, _timed_out) =
        run_with_timeout(&["repl", "--watch"], Duration::from_secs(5));
    assert!(
        stderr.contains("--watch") && stderr.contains("run"),
        "gate error must mention --watch and the runnable subcommands; got: {stderr}"
    );
}
