//! Regression lock for the round-95 cosmetic GAP in the `task.join`
//! main-thread error-prefixing logic (`src/builtins/concurrency.rs`,
//! the `"join"` arm of `call_task`).
//!
//! # The bug (pre-fix)
//!
//! When `main` calls `task.join(h)` and the joinee never completes,
//! `main_thread_wait_for_join` surfaces a
//! `"deadlock on main thread: task.join with no progress possible"`
//! diagnostic. The join arm then UNCONDITIONALLY re-prefixed every
//! `Err` it received with `"joined task failed: "`, producing the
//! conflated, misleading
//!
//! ```text
//! joined task failed: deadlock on main thread: task.join with no progress possible
//! ```
//!
//! That message lies: the joinee did not "fail" — the MAIN thread is
//! starved. "joined task failed" is a condition of the joinee; a
//! main-thread deadlock is a condition of the joiner and must surface on
//! its own. (The historical narrative comment in
//! `tests/docs_round88_supervisor_runs_tests.rs` even shows this exact
//! conflated string as the symptom of a different doc bug.)
//!
//! # The fix
//!
//! The join arm now skips the `"joined task failed: "` prefix when the
//! underlying message starts with `"deadlock on main thread"`. VmError
//! carries no structured kind field (see `src/vm/error.rs`: it is just a
//! `message` plus span/yield metadata), so the discriminator is the
//! stable `"deadlock on main thread"` marker that every diagnostic from
//! `main_thread_wait_for_join` carries verbatim. Genuine joinee failures
//! (a task that errors / is cancelled) are untouched and still show
//! `"joined task failed: <msg>"`.
//!
//! # What these tests lock
//!
//! * `main_join_deadlock_has_no_joined_task_failed_prefix` — the bug
//!   repro. A main-thread join on a task that can never make progress
//!   must surface the clean `"deadlock on main thread"` message WITHOUT
//!   the spurious `"joined task failed:"` prefix. This test FAILS before
//!   the fix (the old conflated message contains both fragments) and
//!   PASSES after.
//! * `genuine_joinee_failure_still_prefixed` — a task that errors
//!   (division by zero) must STILL surface `"joined task failed:"`. This
//!   guards the fix from over-broadly suppressing the prefix.

use std::time::Duration;

use silt::scheduler::test_support::InProcessRunner;

/// **Main-thread join deadlock — no `joined task failed:` prefix.**
///
/// `main` spawns a worker that blocks forever on a channel with no
/// possible sender, then joins it. The worker parks in the wake graph;
/// once it is the only live task and is itself blocked with no runnable
/// counterparty, main's join watchdog confirms starvation and fires
/// `"deadlock on main thread: task.join with no progress possible"`.
///
/// Pre-fix, the join arm wrapped that into
/// `"joined task failed: deadlock on main thread: ..."`. The assertion
/// below — that the message does NOT contain `"joined task failed"` —
/// is the load-bearing check that fails before the fix and passes after.
#[test]
fn main_join_deadlock_has_no_joined_task_failed_prefix() {
    // The worker blocks on `channel.receive` of a channel that nothing
    // can ever send to. After the worker parks, the only live task is
    // blocked with no counterparty, so main's `task.join` watchdog fires
    // the deadlock diagnostic. This routes through the
    // `main_thread_wait_for_join` path in `call_task`'s `"join"` arm —
    // exactly the code whose prefixing logic is under test.
    let src = r#"
import channel
import task

fn main() {
  let stuck = channel.new(0)
  let h = task.spawn(fn() {
    match channel.receive(stuck) {
      Message(_) -> 1
      _ -> 2
    }
  })
  task.join(h)
}
"#;

    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();

    assert!(
        !outcome.timed_out,
        "expected a deadlock diagnostic, not a timeout; outcome={outcome:?}",
    );
    let msg = outcome
        .error_message
        .as_deref()
        .unwrap_or_else(|| panic!("expected an error message; outcome={outcome:?}"));

    // The real condition must surface on its own.
    assert!(
        msg.contains("deadlock on main thread"),
        "expected the 'deadlock on main thread' diagnostic; got: {msg:?}",
    );
    assert!(
        msg.contains("task.join with no progress possible"),
        "expected the join-specific deadlock phrasing; got: {msg:?}",
    );

    // The bug: the misleading joinee-failure prefix must NOT be present.
    assert!(
        !msg.contains("joined task failed"),
        "main-thread join deadlock must NOT be re-prefixed with \
         'joined task failed:' — the joinee did not fail, the main \
         thread is starved. Got conflated message: {msg:?}",
    );
}

/// **Genuine joinee failure — `joined task failed:` prefix preserved.**
///
/// A spawned task that divides by zero genuinely errors. Joining it must
/// still wrap the joinee's error with `"joined task failed:"`. This
/// ensures the deadlock carve-out did not over-broadly suppress the
/// prefix on real joinee failures.
#[test]
fn genuine_joinee_failure_still_prefixed() {
    let src = r#"
import task

fn main() {
  let h = task.spawn(fn() {
    let x = 1 / 0
    x
  })
  task.join(h)
}
"#;

    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();

    assert!(
        !outcome.timed_out,
        "expected a joinee-failure error, not a timeout; outcome={outcome:?}",
    );
    let msg = outcome
        .error_message
        .as_deref()
        .unwrap_or_else(|| panic!("expected an error message; outcome={outcome:?}"));

    assert!(
        msg.contains("joined task failed"),
        "a genuinely failing joinee must still surface 'joined task \
         failed:'; got: {msg:?}",
    );
    assert!(
        !msg.contains("deadlock on main thread"),
        "a division-by-zero joinee is a real failure, not a deadlock; \
         got: {msg:?}",
    );
}
