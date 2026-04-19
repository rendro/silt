//! Regression tests for a scheduler race in `src/scheduler.rs`'s
//! `BlockReason::Send` / `BlockReason::Receive` arms.
//!
//! Bug (pre-fix) introduced by round-27's refactor to
//! `register_{send,recv}_waker_guard`: after registering the waker, the
//! scheduler cloned the task's handle out of `task_slot` for the cancel
//! cleanup closure:
//!
//! ```ignore
//! let reg = ch.register_send_waker_guard(Box::new(move || {
//!     if let Some(task) = slot.lock().take() { requeue(&inner2, task, false); }
//! }));
//! // ← waker may fire IMMEDIATELY if a peer is already parked on the
//! //   rendezvous handshake; firing sets task_slot to None.
//! let handle_for_cancel = task_slot.lock().as_ref()
//!     .expect("task_slot just initialized")   // ← panics: slot is None
//!     .handle.clone();
//! ```
//!
//! `register_{send,recv}_waker_guard` can synchronously invoke the
//! waker closure when a peer is already parked at a rendezvous (the
//! "fire inline during double-check" code path inside the channel's
//! register_* functions). The closure clears `task_slot`. The next
//! line then `expect`s the slot to still be `Some` → panic:
//! `thread '<unnamed>' panicked at src/scheduler.rs:576:30:
//! task_slot just initialized`.
//!
//! Fix: capture `handle_for_cancel` BEFORE calling
//! `register_*_waker_guard`. The slot is initialized immediately
//! above the `match reason { ... }` block and nothing mutates it
//! between there and the register call, so cloning the handle first
//! is safe.
//!
//! Repro: 16-sender fan-in on a rendezvous channel. The bug fired on
//! ~80% of debug runs, so N iterations gives detection probability
//! 1 − 0.2^N. N = 20 → ≥ 99.99%. We also mirror with 16 receivers /
//! 16 senders to stress the Receive arm's identical race.
//!
//! # Phase 2: in-process harness
//!
//! Round-30+ versions shelled out to the silt CLI per trial. Phase 2
//! moves them onto the `silt::scheduler::test_support::InProcessRunner`
//! harness — same Silt source, same assertions, no subprocess
//! overhead. The scheduler-race panic shape (`thread '<unnamed>'
//! panicked at ... task_slot just initialized`) surfaces as
//! `outcome.saw_panic() == true` because the harness uses
//! `catch_unwind` around `vm.run` and turns any unwind into an
//! `error_message: "panic in vm thread: ..."`.

use std::time::Duration;

use silt::scheduler::test_support::{InProcessRunner, TrialOutcome, TrialStats};

/// Assert that the trial produced neither the scheduler race panic nor
/// any generic panic, and didn't time out. Mirrors the subprocess
/// `assert_no_scheduler_panic` shape.
fn assert_no_scheduler_panic(trial: usize, label: &str, outcome: &TrialOutcome) {
    assert!(
        !outcome.timed_out,
        "{label} trial {trial}: TIMEOUT; outcome={outcome:?}",
    );
    let msg = outcome.error_message.as_deref().unwrap_or("");
    assert!(
        !msg.contains("task_slot just initialized"),
        "{label} trial {trial}: SCHEDULER RACE PANIC detected \
         ('task_slot just initialized'); outcome={outcome:?}",
    );
    // saw_panic() catches the catch_unwind-wrapped panic that the
    // harness surfaces from the worker thread.
    assert!(
        !outcome.saw_panic(),
        "{label} trial {trial}: panic detected; outcome={outcome:?}",
    );
}

/// **Send-arm race**: 16 senders fan in on a rendezvous channel; one
/// receiver drains all 16 values. When each sender blocks in
/// `BlockReason::Send`, `register_send_waker_guard` may fire inline
/// because the receiver is already parked at the handshake. The bug:
/// the handle was captured AFTER the register call, and the waker
/// closure (fired inline) had already cleared `task_slot`.
///
/// Sum = 1+2+…+16 = 136.
///
/// Iterations: 20. Pre-fix failure rate is ~80% per run ⇒ per-20-run
/// detection probability ≥ 1 − 0.2^20 ≈ 1 − 1e-14 (effectively 100%).
/// In practice the bug fires within the first 1-2 trials on debug.
#[test]
fn test_send_arm_no_panic_16_sender_fan_in() {
    // The subprocess version printed `sum=136`. The in-process
    // version returns 136 from main and asserts on the returned
    // value — same invariant, no stdout plumbing required. The
    // `senders |> list.each { h -> task.join(h) }` call is preserved
    // because joining the handles after the receive loop is part of
    // the shape the original race manifested under.
    let src = r#"
import channel
import list
import task

fn main() {
  let ch = channel.new(0)
  let senders = 1..16
    |> list.map { i -> task.spawn(fn() { channel.send(ch, i) }) }
  let sum = loop c = 0, acc = 0 {
    match c >= 16 {
      true -> acc
      _ -> {
        match channel.receive(ch) {
          Message(v) -> loop(c + 1, acc + v)
          Closed -> acc
          Empty -> acc
          Sent -> acc
        }
      }
    }
  }
  senders |> list.each { h -> task.join(h) }
  sum
}
"#;
    const ITERATIONS: usize = 20;
    // Phase 3: STRICT 0/20 every trial. The new event-driven
    // watchdog (src/scheduler/wake_graph.rs) signals on every
    // park/wake/spawn/complete so the dequeue-to-register-waker
    // window is no longer race-able by a polling sample.
    const MAX_DEADLOCK_FALSE_POSITIVES: usize = 0;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(2));
    let outcomes: Vec<_> = (0..ITERATIONS).map(|_| runner.run_trial()).collect();
    for (i, o) in outcomes.iter().enumerate() {
        assert_no_scheduler_panic(i, "send-arm fan-in", o);
    }
    let stats = TrialStats::compute(&outcomes, Some(136));
    assert!(
        stats.deadlock_count == MAX_DEADLOCK_FALSE_POSITIVES,
        "send-arm fan-in: {}/{} false-positive deadlock diagnostics \
         (tolerance: {MAX_DEADLOCK_FALSE_POSITIVES}). First failure: \
         idx={:?} msg={:?}",
        stats.deadlock_count,
        ITERATIONS,
        stats.first_failure_index,
        stats.first_failure_message,
    );
    assert_eq!(
        stats.wrong_value_count, 0,
        "send-arm fan-in: {}/{} trials did not reach 136 without \
         deadlock. First failure: idx={:?} msg={:?}",
        stats.wrong_value_count, ITERATIONS, stats.first_failure_index, stats.first_failure_message,
    );
}

/// **Receive-arm race**: symmetric shape. 16 receivers park on a
/// rendezvous channel; 16 senders then hand off values. When each
/// receiver blocks in `BlockReason::Receive`,
/// `register_recv_waker_guard` may fire inline because a sender is
/// already parked on the handshake (depending on interleaving), and
/// the pre-fix code cloned the handle after the register call.
///
/// Each receiver tells us its value via `task.join`; main sums them.
/// Sum should be 136 (1..=16 inclusive).
#[test]
fn test_recv_arm_no_panic_16_receiver_fan_out() {
    let src = r#"
import channel
import list
import task

fn main() {
  let ch = channel.new(0)
  let receivers = 1..16
    |> list.map { _ -> task.spawn(fn() {
      match channel.receive(ch) {
        Message(v) -> v
        Closed -> 0
        Empty -> 0
        Sent -> 0
      }
    }) }
  let senders = 1..16
    |> list.map { i -> task.spawn(fn() { channel.send(ch, i) }) }
  let sum = receivers
    |> list.map { h -> task.join(h) }
    |> list.fold(0) { acc, v -> acc + v }
  senders |> list.each { h -> task.join(h) }
  sum
}
"#;
    const ITERATIONS: usize = 20;
    // Phase 3: STRICT 0/20 every trial. The new event-driven
    // watchdog (src/scheduler/wake_graph.rs) signals on every
    // park/wake/spawn/complete so the dequeue-to-register-waker
    // window is no longer race-able by a polling sample.
    const MAX_DEADLOCK_FALSE_POSITIVES: usize = 0;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(2));
    let outcomes: Vec<_> = (0..ITERATIONS).map(|_| runner.run_trial()).collect();
    for (i, o) in outcomes.iter().enumerate() {
        assert_no_scheduler_panic(i, "recv-arm fan-out", o);
    }
    let stats = TrialStats::compute(&outcomes, Some(136));
    assert!(
        stats.deadlock_count == MAX_DEADLOCK_FALSE_POSITIVES,
        "recv-arm fan-out: {}/{} false-positive deadlock diagnostics \
         (tolerance: {MAX_DEADLOCK_FALSE_POSITIVES}). First failure: \
         idx={:?} msg={:?}",
        stats.deadlock_count,
        ITERATIONS,
        stats.first_failure_index,
        stats.first_failure_message,
    );
    assert_eq!(
        stats.wrong_value_count, 0,
        "recv-arm fan-out: {}/{} trials did not reach 136 without \
         deadlock. First failure: idx={:?} msg={:?}",
        stats.wrong_value_count, ITERATIONS, stats.first_failure_index, stats.first_failure_message,
    );
}
