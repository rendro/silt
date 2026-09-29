//! Round 93 (BLOAT): lock tests for the extraction of the duplicated
//! main-thread race-point re-check blocks in
//! `src/builtins/concurrency.rs`.
//!
//! Before the fix, the ~13-line `match ch.try_send(val.clone())`
//! re-check block appeared 4 times byte-identically inside
//! `main_thread_wait_for_send`'s wait loop (top-of-loop,
//! post-waker-registration, post-starvation-BFS, post-confirm), the
//! `try_receive` equivalent 4 times inside `main_thread_wait_for_receive`,
//! and the `handle.try_get()` analogue 3 times inside
//! `main_thread_wait_for_join`. Those copies are deliberate re-checks at
//! distinct race windows hardened across audit rounds 90-92 — in
//! particular every post-`confirm_main_starved` copy runs BEFORE
//! `confirmed` is tested, so a slot/value/result that races open during
//! the confirm window wins over a deadlock verdict. The risk of the
//! duplication: a future edit to one copy that misses the siblings
//! silently reopens lost-wakeup / deadlock-false-positive bugs.
//!
//! Fix: each family funnels through a single per-function `recheck`
//! closure (markers `ROUND93-RECHECK(send|recv|join)` in the source).
//! The no-scheduler fast paths at the top of each function INTENTIONALLY
//! differ (no waker/park exists yet; Full/Empty/missing-result is an
//! immediate deadlock) and stay separate.
//!
//! Locks:
//!   (a) behavior — rendezvous send/receive across tasks still completes
//!       (N=20 iterations, both directions);
//!   (b) behavior — send- and receive-deadlock are still detected on
//!       BOTH the no-scheduler fast path and the scheduler/wake-graph
//!       path;
//!   (c) source-grep — the re-check logic appears exactly once per
//!       family (one `recheck` closure; the only other `try_send` /
//!       `try_receive` / `try_get` call in each wait function is the
//!       intentionally-distinct fast path), and every former call site
//!       still goes through the closure. A future inline re-duplication
//!       fails these counts loudly.
//!
//! NOTE: like the other scheduler suites, the behavioral tests here are
//! timing-sensitive — run this binary alone, never overlapped with
//! another cargo test invocation (CPU oversubscription causes false
//! failures).

use std::path::PathBuf;
use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;

// ── helpers ──────────────────────────────────────────────────────────

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

fn read_concurrency_src() -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("src/builtins/concurrency.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Slice the source between the first occurrence of `start` and the
/// first occurrence of `end` after it (function-region extraction for
/// the per-family grep locks).
fn region<'a>(src: &'a str, start: &str, end: &str) -> &'a str {
    let s = src
        .find(start)
        .unwrap_or_else(|| panic!("`{start}` not found in src/builtins/concurrency.rs"));
    let e = src[s..]
        .find(end)
        .map(|o| s + o)
        .unwrap_or_else(|| panic!("`{end}` not found after `{start}`"));
    &src[s..e]
}

// ── (a) rendezvous behavior lock ─────────────────────────────────────

/// Main-thread send to a spawned receiver over a rendezvous (capacity-0)
/// channel completes, N=20 iterations. Exercises the send wait loop's
/// recheck path (main blocks until the receiver registers).
#[test]
fn rendezvous_main_send_task_receive_completes_n20() {
    for i in 0..20 {
        let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(0)
  let consumer = task.spawn(fn() {
    let Message(v) = channel.receive(ch)
    v
  })
  channel.send(ch, 42)
  task.join(consumer)
}
    "#);
        assert_eq!(
            result,
            Value::Int(42),
            "iteration {i}: rendezvous send lost"
        );
    }
}

/// Main-thread receive from a spawned sender over a rendezvous channel
/// completes, N=20 iterations. Exercises the receive wait loop's recheck
/// path (main blocks until the sender hands off).
#[test]
fn rendezvous_main_receive_task_send_completes_n20() {
    for i in 0..20 {
        let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(0)
  let producer = task.spawn(fn() {
    channel.send(ch, 7)
  })
  let Message(v) = channel.receive(ch)
  task.join(producer)
  v
}
    "#);
        assert_eq!(
            result,
            Value::Int(7),
            "iteration {i}: rendezvous receive lost"
        );
    }
}

// ── (b) deadlock-detection behavior locks ────────────────────────────

/// Send deadlock, NO-SCHEDULER FAST PATH: no task is ever spawned, so no
/// scheduler exists; a rendezvous send with no counterparty must fire
/// the deadlock diagnostic immediately (the intentionally-separate fast
/// path at the top of `main_thread_wait_for_send`).
#[test]
fn send_deadlock_detected_on_no_scheduler_fast_path() {
    let err = run_err(
        r#"
import channel
fn main() {
  let ch = channel.new(0)
  channel.send(ch, 1)
}
    "#,
    );
    assert!(
        err.contains("deadlock on main thread: channel send with no counterparty"),
        "expected fast-path send deadlock diagnostic, got: {err}"
    );
}

/// Receive deadlock, NO-SCHEDULER FAST PATH: mirror of the send case for
/// `main_thread_wait_for_receive`'s fast path.
#[test]
fn receive_deadlock_detected_on_no_scheduler_fast_path() {
    let err = run_err(
        r#"
import channel
fn main() {
  let ch = channel.new(0)
  channel.receive(ch)
}
    "#,
    );
    assert!(
        err.contains("deadlock on main thread: channel receive with no counterparty"),
        "expected fast-path receive deadlock diagnostic, got: {err}"
    );
}

/// Send deadlock, SCHEDULER / WAKE-GRAPH PATH: a task is spawned (so a
/// scheduler exists) and completes without ever receiving; main's
/// rendezvous send must then go through the wait loop — recheck, park,
/// starvation BFS, confirm gate — and still fire the deadlock
/// diagnostic. Mirrors
/// `scheduler_deadlock_detector_tests::test_real_deadlock_detected_after_spawn_completes_without_sending`
/// for the send direction.
#[test]
fn send_deadlock_detected_on_wake_graph_path() {
    let err = run_err(
        r#"
import channel
import task
fn main() {
  let ch = channel.new(0)
  let h = task.spawn(fn() { 1 })
  task.join(h)
  channel.send(ch, 2)
}
    "#,
    );
    assert!(
        err.contains("deadlock on main thread: channel send with no counterparty"),
        "expected wake-graph send deadlock diagnostic, got: {err}"
    );
}

/// Receive deadlock, SCHEDULER / WAKE-GRAPH PATH: mirror of the send
/// case for `main_thread_wait_for_receive`'s wait loop.
#[test]
fn receive_deadlock_detected_on_wake_graph_path() {
    let err = run_err(
        r#"
import channel
import task
fn main() {
  let ch = channel.new(0)
  let h = task.spawn(fn() { 1 })
  task.join(h)
  channel.receive(ch)
}
    "#,
    );
    assert!(
        err.contains("deadlock on main thread: channel receive with no counterparty"),
        "expected wake-graph receive deadlock diagnostic, got: {err}"
    );
}

// ── (c) source-grep regression locks ─────────────────────────────────

/// SEND family: inside `main_thread_wait_for_send`, the try_send
/// re-check logic must exist in exactly ONE place (the `recheck`
/// closure); the only other `match ch.try_send(` is the
/// intentionally-distinct no-scheduler fast path. All four former copy
/// sites must call the closure. Re-inlining a copy raises the match
/// count and fails here.
#[test]
fn send_recheck_logic_single_source_of_truth() {
    let src = read_concurrency_src();
    let r = region(
        &src,
        "fn main_thread_wait_for_send(",
        "fn main_thread_wait_for_receive(",
    );
    let matches = r.matches("match ch.try_send(").count();
    assert_eq!(
        matches, 2,
        "expected exactly 2 `match ch.try_send(` in main_thread_wait_for_send \
         (the no-scheduler fast path + the ROUND93-RECHECK(send) closure body); \
         found {matches}. Do not re-inline the wait-loop re-check blocks — \
         route every race-point re-check through the `recheck` closure."
    );
    assert_eq!(
        r.matches("let recheck = ").count(),
        1,
        "main_thread_wait_for_send must define exactly one `recheck` closure"
    );
    let calls = r.matches("recheck(&mut reg)").count();
    assert_eq!(
        calls, 4,
        "expected the 4 race-point sites in main_thread_wait_for_send \
         (top-of-loop, post-registration, post-BFS, post-confirm) to call \
         `recheck(&mut reg)`; found {calls} calls"
    );
    // Colon-suffixed needle: the recv/join family comments cross-reference
    // the send marker without a colon ("same rationale as
    // ROUND93-RECHECK(send) in ..."), so only the definition site matches.
    assert_eq!(
        src.matches("ROUND93-RECHECK(send):").count(),
        1,
        "the ROUND93-RECHECK(send): marker comment must appear exactly once"
    );
}

/// RECEIVE family: same lock for `main_thread_wait_for_receive`.
#[test]
fn receive_recheck_logic_single_source_of_truth() {
    let src = read_concurrency_src();
    let r = region(
        &src,
        "fn main_thread_wait_for_receive(",
        "fn main_thread_wait_for_join(",
    );
    let matches = r.matches("match ch.try_receive()").count();
    assert_eq!(
        matches, 2,
        "expected exactly 2 `match ch.try_receive()` in \
         main_thread_wait_for_receive (the no-scheduler fast path + the \
         ROUND93-RECHECK(recv) closure body); found {matches}. Do not \
         re-inline the wait-loop re-check blocks."
    );
    assert_eq!(
        r.matches("let recheck = ").count(),
        1,
        "main_thread_wait_for_receive must define exactly one `recheck` closure"
    );
    let calls = r.matches("recheck(&mut reg)").count();
    assert_eq!(
        calls, 4,
        "expected the 4 race-point sites in main_thread_wait_for_receive to \
         call `recheck(&mut reg)`; found {calls} calls"
    );
    assert_eq!(
        src.matches("ROUND93-RECHECK(recv):").count(),
        1,
        "the ROUND93-RECHECK(recv): marker comment must appear exactly once"
    );
}

/// JOIN family: same lock for `main_thread_wait_for_join`.
#[test]
fn join_recheck_logic_single_source_of_truth() {
    let src = read_concurrency_src();
    let r = region(&src, "fn main_thread_wait_for_join(", "#[cfg(test)]");
    let matches = r.matches("handle.try_get()").count();
    assert_eq!(
        matches, 2,
        "expected exactly 2 `handle.try_get()` in main_thread_wait_for_join \
         (the no-scheduler fast path + the ROUND93-RECHECK(join) closure \
         body); found {matches}. Do not re-inline the wait-loop re-check \
         blocks."
    );
    assert_eq!(
        r.matches("let recheck = ").count(),
        1,
        "main_thread_wait_for_join must define exactly one `recheck` closure"
    );
    let calls = r.matches("recheck()").count();
    assert_eq!(
        calls, 3,
        "expected the 3 race-point sites in main_thread_wait_for_join \
         (top-of-loop, post-BFS, post-confirm) to call `recheck()`; found \
         {calls} calls"
    );
    assert_eq!(
        src.matches("ROUND93-RECHECK(join):").count(),
        1,
        "the ROUND93-RECHECK(join): marker comment must appear exactly once"
    );
}

/// Ordering lock (textual): in each family, the post-confirm re-check
/// must run BEFORE `confirmed` is tested, so a racing slot/value/result
/// wins over a deadlock verdict. We pin the
/// `confirm_main_starved` -> `recheck` -> `if confirmed` sequence per
/// function region.
#[test]
fn post_confirm_recheck_precedes_confirmed_test() {
    let src = read_concurrency_src();
    for (fn_start, fn_end, call) in [
        (
            "fn main_thread_wait_for_send(",
            "fn main_thread_wait_for_receive(",
            "recheck(&mut reg)",
        ),
        (
            "fn main_thread_wait_for_receive(",
            "fn main_thread_wait_for_join(",
            "recheck(&mut reg)",
        ),
        ("fn main_thread_wait_for_join(", "#[cfg(test)]", "recheck()"),
    ] {
        let r = region(&src, fn_start, fn_end);
        let confirm = r
            .find("let confirmed = confirm_main_starved(")
            .unwrap_or_else(|| panic!("{fn_start} lost its confirm_main_starved gate"));
        let after_confirm = &r[confirm..];
        let recheck_pos = after_confirm
            .find(call)
            .unwrap_or_else(|| panic!("{fn_start}: no `{call}` after confirm_main_starved"));
        let confirmed_pos = after_confirm
            .find("if confirmed {")
            .unwrap_or_else(|| panic!("{fn_start}: no `if confirmed` after confirm_main_starved"));
        assert!(
            recheck_pos < confirmed_pos,
            "{fn_start}: the post-confirm `{call}` must run BEFORE `if \
             confirmed` is tested (a slot/value/result that races open \
             during the confirm window must win over the deadlock verdict); \
             found recheck at +{recheck_pos}, `if confirmed` at +{confirmed_pos}"
        );
    }
}
