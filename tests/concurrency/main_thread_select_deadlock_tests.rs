//! Regression lock: a main-thread `channel.select` over an op set with
//! no possible counterparty and no other live tasks must report a
//! "deadlock on main thread" error within the confirm window — NOT spin
//! forever.
//!
//! Before the fix, the `channel.select` builtin's main-thread branch was
//! left on the legacy polling loop (`cvar.wait_for(1s)` forever). It
//! never called `install_main_signal` / `park_main` / `main_thread_is_starved`,
//! so a select with no sender just looped on a benign 1s poll and never
//! returned an error — whereas the equivalent `channel.receive(ch)` /
//! `channel.send(ch, v)` programs DO report a deadlock.
//!
//! The fix routes the main-thread select branch through the same
//! wake-graph-driven `main_thread_wait_for_select` protocol the
//! receive/send/join paths use. These tests mirror
//! `scheduler_deadlock_detector_tests::test_real_deadlock_still_detected`
//! but exercise the select path.
//!
//! NOTE: with the pre-fix polling loop, `run_trial()` returns
//! `timed_out == true` (the budget elapses) and `saw_deadlock()` is
//! false — so the assertions below FAIL before the fix and PASS after.

use std::time::Duration;

use silt::scheduler::test_support::InProcessRunner;

/// A bare receive-arm select on an unbuffered channel with no sender and
/// no other live task: the wake graph proves no counterparty can ever
/// drive the arm ready, so the main thread must report a deadlock.
#[test]
fn main_thread_select_recv_no_counterparty_reports_deadlock() {
    let src = r#"
import channel

fn main() {
  let ch = channel.new(0)
  match channel.select([Recv(ch)]) {
    (_, Message(_)) -> 1
    _ -> 2
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(3));
    let outcome = runner.run_trial();
    assert!(
        !outcome.timed_out,
        "main-thread select with no counterparty must report a deadlock, \
         not spin until the budget elapses; outcome={outcome:?}",
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

/// A send-arm select on a full (capacity-0, no receiver) channel with no
/// other live task: same deadlock proof from the send side.
#[test]
fn main_thread_select_send_no_counterparty_reports_deadlock() {
    let src = r#"
import channel

fn main() {
  let ch = channel.new(0)
  match channel.select([Send(ch, 42)]) {
    (_, Sent) -> 1
    _ -> 2
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(3));
    let outcome = runner.run_trial();
    assert!(
        !outcome.timed_out,
        "main-thread select send with no counterparty must report a deadlock, \
         not spin until the budget elapses; outcome={outcome:?}",
    );
    assert!(
        outcome.saw_deadlock(),
        "expected the 'deadlock on main thread' diagnostic; outcome={outcome:?}",
    );
    assert!(
        outcome.result.is_none(),
        "main must not have produced a value; outcome={outcome:?}",
    );
}
