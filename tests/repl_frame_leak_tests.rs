//! Regression locks for the REPL "phantom call frame" leak.
//!
//! Background: `Vm::run` pushes a top-level call frame and then enters
//! `execute()`. On the success path, `Op::Return` pops the frame back
//! off, but on the *error* path nothing did — the frame (and any deeper
//! frames the unwinding error left behind) stayed on the persistent VM.
//! The next REPL evaluation pushed a new frame on top, and when its own
//! error fired, `enrich_error` walked the entire (stale + fresh) frame
//! list to build the call stack. The user saw misleading `-> main at
//! <declaration>` rows from prior REPL turns, and the chain grew once
//! per error.
//!
//! Fix (src/vm/mod.rs::Vm::run): snapshot `frames.len()`, `stack.len()`,
//! and `tco_elided.len()` before pushing the script frame; on the `Err`
//! branch, build the enriched error first (so it captures the real
//! call stack for THIS run) and *then* truncate the three buffers back
//! to the snapshot. Stale state can't leak into the next `run` call.
//!
//! These tests intentionally exercise the bug end-to-end through
//! `silt repl` as a subprocess. Unit-level VM tests don't catch the
//! leak because the leak is across *successive* `run` calls on a
//! persistent VM — exactly what the REPL does and unit tests typically
//! don't.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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
    #[allow(dead_code)]
    stdout: String,
    stderr: String,
}

/// Run `silt repl` with `script` written to stdin (and `:quit\n` always
/// appended) and return captured stdout/stderr. A hard timeout guards
/// against a hung REPL hanging the test runner.
fn run_session(script: &str) -> SessionOutput {
    let mut child: Child = silt_cmd()
        .spawn()
        .expect("failed to spawn `silt repl` subprocess");

    {
        let stdin = child.stdin.as_mut().expect("child stdin was not piped");
        stdin
            .write_all(script.as_bytes())
            .expect("failed to write script");
        if !script.ends_with('\n') {
            stdin.write_all(b"\n").expect("failed to write newline");
        }
        stdin.write_all(b":quit\n").expect("failed to write :quit");
    }
    drop(child.stdin.take());

    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(SESSION_TIMEOUT) {
        Ok(Ok(out)) => {
            let _ = handle.join();
            SessionOutput {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            }
        }
        Ok(Err(e)) => panic!("failed to wait on repl child: {e}"),
        Err(_) => panic!(
            "repl session did not exit within {}s",
            SESSION_TIMEOUT.as_secs()
        ),
    }
}

/// Split the REPL's stderr into one chunk per `error[runtime]:` header.
/// Each chunk is the header line plus every line up to (but not
/// including) the next header. Empty input → empty result.
fn split_runtime_errors(stderr: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in stderr.lines() {
        if line.starts_with("error[runtime]:") {
            if let Some(prev) = current.take() {
                out.push(prev);
            }
            current = Some(String::from(line));
        } else if let Some(buf) = current.as_mut() {
            buf.push('\n');
            buf.push_str(line);
        }
        // Lines before the first `error[runtime]:` header are dropped
        // (e.g. type-error stderr from earlier inputs).
    }
    if let Some(last) = current {
        out.push(last);
    }
    out
}

/// Count the `-> <name> at <loc>` call-stack lines inside one error
/// chunk. These are the lines `render_call_stack` emits for each
/// frame of the rendered stack.
fn count_call_stack_frames(error_chunk: &str) -> usize {
    error_chunk
        .lines()
        .filter(|l| l.trim_start().starts_with("-> "))
        .count()
}

/// Bare-expression repro from the bug report.
///
/// Two `1/0` errors evaluated as expressions. Each gets wrapped in
/// `fn main()` by the REPL, so each error's call stack is a single
/// `main` frame — `render_call_stack` filters single-frame stacks
/// out entirely (see vm/error.rs). The leak therefore manifests on
/// the SECOND error: prior to the fix, stale frames from the first
/// `run` made the filtered stack length ≥ 2, and phantom `-> main`
/// lines appeared. After the fix, neither error renders any
/// call-stack frames.
#[test]
fn repl_bare_expression_errors_have_no_leaked_frames() {
    let session = run_session("1/0\n1/0\n");
    let errors = split_runtime_errors(&session.stderr);
    assert_eq!(
        errors.len(),
        2,
        "expected exactly 2 runtime errors, got {}: {:#?}",
        errors.len(),
        errors
    );
    for (i, err) in errors.iter().enumerate() {
        let frames = count_call_stack_frames(err);
        assert_eq!(
            frames, 0,
            "error #{i} leaked {frames} phantom call-stack frame(s):\n---\n{err}\n---"
        );
    }
}

/// Named-function repro from the bug report.
///
/// Each runtime error's call stack should contain ONLY the frames
/// belonging to that error's script. Before the fix, the second
/// error rendered `err2 -> main -> err1 -> main` — phantom `err1`
/// and second `main` frames bled in from the first error. After the
/// fix, each error renders exactly its own `<fn> -> main` chain
/// (2 frames), and the count never grows across successive errors.
#[test]
fn repl_named_function_errors_render_only_own_frames() {
    let script = "\
fn err1(){1/0}
fn err2(){1/0}
err1()
err2()
";
    let session = run_session(script);
    let errors = split_runtime_errors(&session.stderr);
    assert_eq!(
        errors.len(),
        2,
        "expected exactly 2 runtime errors, got {}: {:#?}",
        errors.len(),
        errors
    );

    let frames_first = count_call_stack_frames(&errors[0]);
    let frames_second = count_call_stack_frames(&errors[1]);

    // Each error should render exactly its own `<fn>` and `main` —
    // 2 frames. Anything more means stale frames leaked.
    assert_eq!(
        frames_first, 2,
        "first error should render 2 call-stack frames (err1 -> main), got {frames_first}:\n---\n{}\n---",
        errors[0]
    );
    assert_eq!(
        frames_second, 2,
        "second error should render 2 call-stack frames (err2 -> main), got {frames_second}:\n---\n{}\n---",
        errors[1]
    );

    // Phantom-frame check: the second error must not name `err1`,
    // because `err1` belongs to a prior REPL turn. Before the fix,
    // the chain rendered as `err2 -> main -> err1 -> main`.
    assert!(
        !errors[1].contains("err1"),
        "second error must not reference `err1` from a prior turn:\n---\n{}\n---",
        errors[1]
    );
    // Symmetric sanity check: the first error must reference `err1`
    // (that's the function that actually raised it) but NOT `err2`.
    assert!(
        errors[0].contains("err1"),
        "first error should mention err1 (it raised the error):\n---\n{}\n---",
        errors[0]
    );
    assert!(
        !errors[0].contains("err2"),
        "first error should not mention err2:\n---\n{}\n---",
        errors[0]
    );

    // Frame count must not grow turn over turn.
    assert!(
        frames_second <= frames_first,
        "frame count grew across errors ({frames_first} -> {frames_second}); \
         indicates stale frames leaking:\nerr1 chunk:\n{}\n\nerr2 chunk:\n{}",
        errors[0],
        errors[1]
    );
}

/// Frame count must not grow without bound across N successive
/// runtime errors. Before the fix, each error left its `main` frame
/// behind and the next error's call stack picked up one extra
/// phantom frame, so the rendered stack grew linearly.
///
/// We use named functions (so `render_call_stack` doesn't filter
/// the single-`main` stack to empty) and assert that every error
/// after the first renders the SAME 2-frame stack.
#[test]
fn repl_frame_count_does_not_grow_across_many_errors() {
    let script = "\
fn boom(){1/0}
boom()
boom()
boom()
boom()
boom()
";
    let session = run_session(script);
    let errors = split_runtime_errors(&session.stderr);
    assert_eq!(
        errors.len(),
        5,
        "expected 5 runtime errors, got {}: {:#?}",
        errors.len(),
        errors
    );

    let counts: Vec<usize> = errors.iter().map(|e| count_call_stack_frames(e)).collect();
    let first = counts[0];
    assert_eq!(
        first, 2,
        "first error should render 2 frames (boom -> main), got {first}:\n{:#?}",
        errors
    );
    for (i, &c) in counts.iter().enumerate() {
        assert_eq!(
            c, first,
            "error #{i} rendered {c} frames, expected {first} — phantom frames leaking. \
             All counts: {counts:?}\n\nFull errors:\n{:#?}",
            errors
        );
    }
}
