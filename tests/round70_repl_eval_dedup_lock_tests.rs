//! Round-70 audit lock: REPL evaluator deduplication.
//!
//! Background: `eval_declaration` and `eval_expression` in `src/repl.rs`
//! historically carried near-identical inline blocks for:
//!
//!   * the runtime-error rendering branch (`Err(e) = vm.run(script)`),
//!     covering in-range span, out-of-range span, and span-less paths
//!     plus call-stack rendering; and
//!
//!   * the "internal error: empty function list" guard after popping
//!     the script function out of the compiler output.
//!
//! Round-70 collapsed both into shared helpers — `render_repl_vm_error`
//! and `take_first_function` — so a future drift between the two REPL
//! paths can't silently re-create the duplication.
//!
//! These tests pin two things:
//!
//!   1. Source-grep lock: each helper is referenced from BOTH
//!      `eval_declaration` and `eval_expression`. If a future change
//!      re-inlines either site, the call-site count drops below 2 and
//!      the lock fails — catching the regression statically without
//!      needing to re-run a behavioral fixture.
//!
//!   2. Behavioural lock: drive `silt repl` as a subprocess with one
//!      input that triggers a runtime error inside a *declaration*'s
//!      `let` initializer (un-wrapped path) and one that triggers a
//!      runtime error inside a bare *expression* (wrapped-in-`fn main`
//!      path). Both must still render the canonical `error[runtime]:`
//!      header and the `<declaration>` locator/call-stack frames the
//!      old inline blocks emitted.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const SESSION_TIMEOUT: Duration = Duration::from_secs(15);

// ── Source-grep lock ───────────────────────────────────────────────

/// The source of `src/repl.rs` is read at compile time. Any drift in
/// either helper's call-site count is caught statically without needing
/// to re-run the behavioral fixture.
const REPL_SRC: &str = include_str!("../src/repl.rs");

/// Count substring occurrences (non-overlapping). `str::matches` already
/// does this; we wrap it so the callers read clearly.
fn occurrences(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

#[test]
fn round70_render_repl_vm_error_helper_is_defined_and_called_twice() {
    // Definition exists.
    assert!(
        REPL_SRC.contains("fn render_repl_vm_error("),
        "expected `fn render_repl_vm_error(` to be defined in src/repl.rs"
    );

    // Called from `eval_declaration` AND `eval_expression`. The helper
    // is invoked as `render_repl_vm_error(`; counting that token gives
    // us (call-sites + 1 definition). We assert exactly 3 occurrences
    // (1 def + 2 call sites). If either branch re-inlines the renderer,
    // this drops to 2 and the lock fires.
    let total = occurrences(REPL_SRC, "render_repl_vm_error(");
    assert_eq!(
        total, 3,
        "expected exactly 3 occurrences of `render_repl_vm_error(` \
         (1 definition + 2 call sites in eval_declaration & eval_expression), \
         got {total}. A drift here usually means one branch re-inlined the renderer."
    );
}

#[test]
fn round70_take_first_function_helper_is_defined_and_called_twice() {
    // Definition exists.
    assert!(
        REPL_SRC.contains("fn take_first_function"),
        "expected `fn take_first_function` to be defined in src/repl.rs"
    );

    // Called from `eval_declaration` AND `eval_expression`. Both call
    // sites use `take_first_function(functions)`; counting the call-form
    // token gives us (call-sites). We assert exactly 2 call sites.
    let calls = occurrences(REPL_SRC, "take_first_function(functions)");
    assert_eq!(
        calls, 2,
        "expected exactly 2 call sites of `take_first_function(functions)` \
         (eval_declaration & eval_expression), got {calls}. A drift here \
         means one branch re-inlined the empty-function-list guard."
    );
}

#[test]
fn round70_old_inline_empty_function_list_guard_does_not_recur() {
    // The previous inline form in `eval_declaration` and `eval_expression`
    // was:
    //
    //     let Some(script) = functions.into_iter().next() else {
    //         eprintln!("internal error: empty function list");
    //         return;
    //     };
    //
    // After dedup, the literal `eprintln!("internal error: empty function list")`
    // lives only inside `take_first_function`. The literal string ALSO appears
    // inside the `#[cfg(test)]` helper `eval_expression_value` (which returns
    // it as a `String` error rather than printing — different surface, doesn't
    // share the helper). So we expect EXACTLY 2 occurrences of the bare string:
    //   * 1 inside `take_first_function` (the eprintln)
    //   * 1 inside `eval_expression_value` (the .ok_or_else literal)
    //
    // A third occurrence indicates one of `eval_declaration` or
    // `eval_expression` re-inlined the eprintln guard.
    let count = occurrences(REPL_SRC, "internal error: empty function list");
    assert_eq!(
        count, 2,
        "expected exactly 2 occurrences of `internal error: empty function list` \
         (inside `take_first_function` and `eval_expression_value`), got {count}. \
         A third occurrence indicates the guard was re-inlined into one of the \
         interactive eval paths."
    );

    // Belt-and-braces: the eprintln form must appear exactly once. If a
    // re-inline adds a second eprintln, the count above would be 3, but
    // we also catch the case where someone copies just the eprintln line.
    let eprintln_count = occurrences(
        REPL_SRC,
        "eprintln!(\"internal error: empty function list\")",
    );
    assert_eq!(
        eprintln_count, 1,
        "expected exactly 1 `eprintln!(\"internal error: empty function list\")` \
         (inside `take_first_function`), got {eprintln_count}."
    );
}

// ── Behavioural lock ───────────────────────────────────────────────

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
        stdin
            .write_all(b":quit\n")
            .expect("failed to write :quit");
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

#[test]
fn round70_expression_runtime_error_renders_canonical_runtime_header() {
    // Bare expression `1/0` — `eval_expression` path (input wrapped in
    // `fn main() { … }`). The shared helper must render the canonical
    // `error[runtime]:` header.
    let session = run_session("1/0\n");
    assert!(
        session.stderr.contains("error[runtime]:"),
        "expected `error[runtime]:` header in expression runtime error stderr, got:\n{}",
        session.stderr
    );
    // And it must not leak the internal `VM error:` prefix from
    // `VmError::Display`.
    assert!(
        !session.stderr.contains("VM error:"),
        "stderr leaked internal `VM error:` prefix:\n{}",
        session.stderr
    );
}

#[test]
fn round70_declaration_runtime_error_calls_render_helper_and_emits_call_stack() {
    // Three declarations: two fns that divide by zero through a chain,
    // then a `let` whose initializer calls the chain. The `let` is the
    // trigger — its runtime error originates inside `deep`, surfaces
    // through `shallow`, so the shared call-stack renderer must emit
    // at least two `-> <name> at <declaration>` lines (single-frame
    // stacks are filtered by `render_call_stack`, so we need depth ≥ 2).
    //
    // This exercises:
    //   * un-wrapped declaration path (eval_declaration)
    //   * `render_repl_vm_error` with `adjust = None` (the declaration
    //     branch passes `None` since spans are already in input coords)
    //   * call-stack rendering against `<declaration>` labels
    let script = "\
fn deep(){1/0}
fn shallow(){deep()}
let x: int = shallow()
";
    let session = run_session(script);
    assert!(
        session.stderr.contains("error[runtime]:"),
        "expected `error[runtime]:` header for declaration-path error, got stderr:\n{}",
        session.stderr
    );
    assert!(
        session.stderr.contains("<declaration>"),
        "expected `<declaration>` locator/frame label in declaration-path error, got stderr:\n{}",
        session.stderr
    );
    // Call-stack frame lines: at least two `-> <name> at <loc>` lines
    // emitted by `render_call_stack` (single-frame stacks are filtered).
    let frame_lines: Vec<&str> = session
        .stderr
        .lines()
        .filter(|l| l.trim_start().starts_with("-> "))
        .collect();
    assert!(
        frame_lines.len() >= 2,
        "expected ≥2 call-stack frame lines (`-> … at …`) for declaration path, \
         got {} ({:?}). stderr:\n{}",
        frame_lines.len(),
        frame_lines,
        session.stderr
    );
    // Each frame label must be `<declaration>` (the closure passed by
    // `render_repl_vm_error` rewrites every frame to that label).
    for line in &frame_lines {
        assert!(
            line.contains("<declaration>"),
            "frame line did not carry `<declaration>` label: {line:?}"
        );
    }
}
