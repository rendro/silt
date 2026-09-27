//! Regression lock: a main-thread `channel.each` over a channel with
//! no possible counterparty must report a "deadlock on main thread"
//! error within the confirm window — NOT hang forever.
//!
//! Before the fix, `channel.each`'s `Empty` arm on the main thread
//! called `ch.receive_blocking()` directly (src/builtins/concurrency.rs,
//! `call_channel`'s "each" arm) — an infinite parking_lot condvar wait
//! with no wake-graph `park_main`, no starvation BFS, and no
//! confirm-stable gate. The equivalent `channel.receive(ch)` /
//! `channel.send(ch, v)` / `channel.select([...])` / `task.join(h)`
//! programs all DO report a deadlock. Same class as the round-2
//! `channel.select` fix locked by `tests/main_thread_select_deadlock_tests.rs`
//! — `channel.each` was the arm left behind.
//!
//! The fix routes the main-thread `each` `Empty` arm through
//! `main_thread_wait_for_receive`, inheriting the no-scheduler fast
//! path, wake-graph park/unpark, waker-registration guard, and
//! confirm-stable gate.
//!
//! NOTE: with the pre-fix blocking wait, `run_trial()` returns
//! `timed_out == true` (the budget elapses) and `saw_deadlock()` is
//! false — so the deadlock assertions below FAIL before the fix and
//! PASS after. The positive-control test locks that the live-
//! counterparty behavior is unchanged.

use std::time::Duration;

use silt::scheduler::test_support::InProcessRunner;

/// `channel.each` on the main thread with no scheduler and no possible
/// sender: the no-scheduler fast path must fire the deadlock
/// diagnostic immediately instead of blocking forever.
#[test]
fn main_thread_each_no_counterparty_reports_deadlock() {
    let src = r#"
import channel

fn main() {
  let ch = channel.new(0)
  channel.each(ch) { msg -> println("{msg}") }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(3));
    let outcome = runner.run_trial();
    assert!(
        !outcome.timed_out,
        "main-thread channel.each with no counterparty must report a deadlock, \
         not hang until the budget elapses; outcome={outcome:?}",
    );
    assert!(
        outcome.saw_deadlock(),
        "expected the 'deadlock on main thread' diagnostic; outcome={outcome:?}",
    );
    assert!(
        outcome
            .error_message
            .as_deref()
            .map(|m| m.contains("no counterparty"))
            .unwrap_or(false),
        "expected the 'no counterparty' phrase; outcome={outcome:?}",
    );
    assert!(
        outcome.result.is_none(),
        "main must not have produced a value; outcome={outcome:?}",
    );
}

/// Wake-graph path: a scheduler exists (a task was spawned and joined)
/// but no live task can ever send — the starvation BFS plus the
/// confirm-stable gate must fire the deadlock instead of parking main
/// forever. Mirrors `receive_deadlock_detected_on_wake_graph_path` in
/// `tests/round93_concurrency_recheck_extraction_tests.rs`.
#[test]
fn main_thread_each_deadlock_detected_on_wake_graph_path() {
    let src = r#"
import channel
import task

fn main() {
  let ch = channel.new(0)
  let h = task.spawn(fn() { 1 })
  task.join(h)
  channel.each(ch) { msg -> println("{msg}") }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(8));
    let outcome = runner.run_trial();
    assert!(
        !outcome.timed_out,
        "main-thread channel.each with a drained scheduler must report a \
         deadlock, not hang until the budget elapses; outcome={outcome:?}",
    );
    assert!(
        outcome.saw_deadlock(),
        "expected the 'deadlock on main thread' diagnostic; outcome={outcome:?}",
    );
}

/// Positive control: with a LIVE counterparty the rewired wait must be
/// behavior-preserving — a spawned producer sends 3 values then closes,
/// and the main-thread `channel.each` drains all 3 (in order, capacity-0
/// rendezvous) and returns cleanly when the channel closes.
///
/// The in-process harness leaves `TrialOutcome::stdout` empty by design
/// (see `compile_and_run` in src/scheduler/test_support.rs), so instead
/// of printing, the callback forwards each drained value into a buffered
/// `results` channel and `main` folds them into a single digit-encoded
/// Int (((0·10+1)·10+2)·10+3 = 123) that also proves the drain ORDER.
#[test]
fn main_thread_each_with_live_producer_drains_all_and_exits_cleanly() {
    let src = r#"
import channel
import task

fn main() {
  let ch = channel.new(0)
  let results = channel.new(8)
  let producer = task.spawn(fn() {
    channel.send(ch, 1)
    channel.send(ch, 2)
    channel.send(ch, 3)
    channel.close(ch)
  })
  channel.each(ch) { n -> channel.send(results, n) }
  task.join(producer)
  loop c = 0, acc = 0 {
    match c >= 3 {
      true -> acc
      _ -> match channel.receive(results) {
        Message(v) -> loop(c + 1, acc * 10 + v)
        _ -> acc
      }
    }
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(10));
    let outcome = runner.run_trial();
    assert!(
        outcome.ok(),
        "channel.each with a live producer must complete cleanly; outcome={outcome:?}",
    );
    assert_eq!(
        outcome.result,
        Some(silt::value::Value::Int(123)),
        "channel.each must drain all 3 rendezvous sends in order \
         (digit-encoded 123); outcome={outcome:?}",
    );
}
