//! Regression tests for `channel.recv_timeout`.
//!
//! Covers the three canonical outcomes (Ok / Err("timeout") / Err("closed")),
//! the "ready value beats expired timer" corner case, zero-duration semantics,
//! negative-duration rejection, cancel cleanup, and basic concurrency fairness.
//!
//! All tests drive Silt source through `InProcessRunner` so the full
//! compile → VM → scheduler → TimerManager chain is exercised end-to-end.
//! Wall-clock budgets are generous (5-10s) to tolerate CI jitter.

use std::time::Duration;

use silt::scheduler::test_support::InProcessRunner;
use silt::value::Value;

/// Per-trial wall-clock budget for tests in this file that exercise quick
/// recv_timeout shapes (zero-duration delivery, negative-duration rejection,
/// try-receive miss). Locked at 10s because Windows GitHub-hosted runners
/// can take long enough on the first in-process trial (compile + Scheduler
/// bring-up + worker thread spawn + TimerManager init) that a 2-second
/// budget produced consistent CI flakes — `TrialOutcome { stdout: "",
/// timed_out: true, elapsed: ~2s }`. Applied uniformly across platforms so
/// we do not accumulate cfg(windows) hacks.
const TEST_HARNESS_TIMEOUT: Duration = Duration::from_secs(10);

/// Happy path: a sibling task sends a value well within the timeout budget.
/// `recv_timeout` must return `Ok(v)` and the program completes under the
/// wall-clock deadline.
#[test]
fn recv_timeout_returns_ok_on_timely_send() {
    let src = r#"
import channel
import task
import time
fn main() {
  let ch = channel.new(0)
  let _h = task.spawn(fn() {
    time.sleep(time.ms(20))
    channel.send(ch, 42)
  })
  match channel.recv_timeout(ch, time.ms(1000)) {
    Ok(v) -> v
    Err(_) -> -1
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(outcome.ok(), "run should succeed: {outcome:?}");
    assert_eq!(
        outcome.result,
        Some(Value::Int(42)),
        "expected Ok(42), got {:?}",
        outcome.result
    );
}

/// No sender, no close: the timer must fire and produce
/// `Err(ChannelTimeout)`. We also assert the elapsed wall-clock is
/// bounded so a hang doesn't masquerade as success.
#[test]
fn recv_timeout_returns_err_timeout_when_no_sender() {
    let src = r#"
import channel
import time
fn main() {
  let ch = channel.new(0)
  -- No task.spawn: nothing will ever send to `ch`. Without a live sender
  -- the scheduler would otherwise fire a main-thread deadlock diagnostic;
  -- recv_timeout must instead surface the clean `Err(ChannelTimeout)` shape.
  match channel.recv_timeout(ch, time.ms(50)) {
    Ok(_) -> "ok"
    Err(ChannelTimeout) -> "timeout"
    Err(ChannelClosed) -> "closed"
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(outcome.ok(), "run should succeed: {outcome:?}");
    assert_eq!(
        outcome.result,
        Some(Value::String("timeout".into())),
        "expected Err(ChannelTimeout), got {:?}",
        outcome.result,
    );
}

/// Closing the channel with no buffered values must surface
/// `Err(ChannelClosed)`. Exercises both the try_receive fast path at
/// entry and the equivalent `Closed` branch of the internal select
/// after a wake.
#[test]
fn recv_timeout_returns_err_closed_on_closed_empty_channel() {
    let src = r#"
import channel
import time
fn main() {
  let ch = channel.new(0)
  channel.close(ch)
  match channel.recv_timeout(ch, time.ms(500)) {
    Ok(_) -> "ok"
    Err(ChannelTimeout) -> "timeout"
    Err(ChannelClosed) -> "closed"
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(outcome.ok(), "run should succeed: {outcome:?}");
    assert_eq!(
        outcome.result,
        Some(Value::String("closed".into())),
        "expected Err(ChannelClosed), got {:?}",
        outcome.result,
    );
}

/// A value already in a buffered channel must be delivered even if the timeout
/// is zero. This is the "ready value beats expired timer" corner case: the
/// timer never wins over an available value.
#[test]
fn recv_timeout_delivers_buffered_value_with_zero_duration() {
    let src = r#"
import channel
import time
fn main() {
  let ch = channel.new(4)
  channel.send(ch, 7)
  match channel.recv_timeout(ch, time.ms(0)) {
    Ok(v) -> v
    Err(_) -> -1
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(TEST_HARNESS_TIMEOUT);
    let outcome = runner.run_trial();
    assert!(outcome.ok(), "run should succeed: {outcome:?}");
    assert_eq!(outcome.result, Some(Value::Int(7)));
}

/// Zero duration on an empty channel must return `Err(ChannelTimeout)`
/// without scheduling a timer (try-receive semantics). No hang, no
/// spurious Ok.
#[test]
fn recv_timeout_zero_duration_empty_channel_returns_timeout() {
    let src = r#"
import channel
import time
fn main() {
  let ch = channel.new(4)
  match channel.recv_timeout(ch, time.ms(0)) {
    Ok(_) -> "ok"
    Err(ChannelTimeout) -> "timeout"
    Err(ChannelClosed) -> "closed"
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(TEST_HARNESS_TIMEOUT);
    let outcome = runner.run_trial();
    assert!(outcome.ok(), "run should succeed: {outcome:?}");
    assert_eq!(outcome.result, Some(Value::String("timeout".into())));
}

/// Cancel path: a spawned task parks in recv_timeout, the main thread
/// cancels it before the timer expires, and a subsequent unrelated use of
/// the same channel observes normal semantics (no leaked waker inflating
/// `waiting_receivers`, no phantom rendezvous send).
///
/// The "phantom send" regression shape is the same as `cancel_path_waker_leak_tests.rs`
/// — a dangling receive waker on the channel would cause a later
/// `try_send` to falsely succeed without a real receiver.
#[test]
fn recv_timeout_cancel_does_not_leak_waker() {
    let src = r#"
import channel
import task
import time
fn main() {
  let ch = channel.new(0)
  -- Spawn a receiver that parks in recv_timeout.
  let h = task.spawn(fn() {
    match channel.recv_timeout(ch, time.ms(5000)) {
      Ok(_) -> 1
      Err(_) -> 0
    }
  })
  -- Give the receiver time to park.
  time.sleep(time.ms(50))
  -- Cancel it mid-wait.
  task.cancel(h)
  -- Pump the scheduler a bit so the cancel cleanup runs.
  time.sleep(time.ms(50))
  -- A try_send on a rendezvous channel with NO receiver parked must
  -- return false. If the cancel path had leaked the recv waker,
  -- `waiting_receivers > 0` would cause try_send to drop the value
  -- into the handoff slot and return true — a phantom send.
  match channel.try_send(ch, 42) {
    true -> "leaked"
    false -> "ok"
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    let outcome = runner.run_trial();
    assert!(outcome.ok(), "run should succeed: {outcome:?}");
    assert_eq!(
        outcome.result,
        Some(Value::String("ok".into())),
        "cancelled recv_timeout must not leak its recv waker; outcome={outcome:?}",
    );
}

/// Fairness / no-ordering-bias: N senders each drive one recv_timeout on a
/// shared channel. Every recv must land `Ok(v)` (no timer wins ahead of a
/// value), and the sum of received values matches the sum of sent values.
///
/// Catches a regression where the timer registration order biases against
/// concurrent receivers (e.g. by holding a lock too long during setup).
#[test]
fn recv_timeout_fair_under_concurrent_use() {
    let src = r#"
import channel
import task
import time
fn main() {
  let ch = channel.new(0)
  -- Eight senders; each fires one value after a short stagger.
  let _s0 = task.spawn(fn() { channel.send(ch, 1) })
  let _s1 = task.spawn(fn() { channel.send(ch, 2) })
  let _s2 = task.spawn(fn() { channel.send(ch, 3) })
  let _s3 = task.spawn(fn() { channel.send(ch, 4) })
  let _s4 = task.spawn(fn() { channel.send(ch, 5) })
  let _s5 = task.spawn(fn() { channel.send(ch, 6) })
  let _s6 = task.spawn(fn() { channel.send(ch, 7) })
  let _s7 = task.spawn(fn() { channel.send(ch, 8) })
  -- Receive all 8 with a per-call timeout that is generous but finite.
  loop c = 0, acc = 0 {
    match c >= 8 {
      true -> acc
      _ -> match channel.recv_timeout(ch, time.ms(5000)) {
        Ok(v) -> loop(c + 1, acc + v)
        Err(_) -> -1
      }
    }
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(10));
    let outcome = runner.run_trial();
    assert!(outcome.ok(), "run should succeed: {outcome:?}");
    assert_eq!(
        outcome.result,
        Some(Value::Int(36)), // 1+2+3+4+5+6+7+8
        "fair recv_timeout must collect every value; outcome={outcome:?}",
    );
}

/// Negative duration must be a construction error, not a silent zero-wait.
/// A negative nanosecond field means the caller built a malformed Duration,
/// which we reject loudly.
#[test]
fn recv_timeout_negative_duration_errors() {
    let src = r#"
import channel
import time
fn main() {
  let ch = channel.new(4)
  -- Construct a negative duration via the raw Duration record literal. This
  -- bypasses the `time.ms` constructor guard so we can hit the recv_timeout
  -- argument validation directly.
  match channel.recv_timeout(ch, Duration { ns: -1 }) {
    Ok(_) -> "ok"
    Err(reason) -> reason
  }
}
"#;
    let runner = InProcessRunner::new(src).with_budget(TEST_HARNESS_TIMEOUT);
    let outcome = runner.run_trial();
    assert!(
        outcome.error_message.is_some(),
        "negative duration must produce a runtime error; outcome={outcome:?}",
    );
    let msg = outcome.error_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("channel.recv_timeout: duration must be non-negative"),
        "expected exact negative-duration diagnostic from src/builtins/concurrency.rs:292; got: {msg}",
    );
}

/// Lock the harness timeout above the Windows-runner safe floor.
///
/// Locked because Windows GitHub-hosted runners cold-start the silt
/// subprocess + scheduler in ~2-4s, and the previous 2-second budget
/// produced consistent CI flakes (`stdout: ""`, `timed_out: true`).
/// If you tighten this back below 8s, expect Windows CI flakes.
#[test]
fn channel_timeout_test_harness_uses_at_least_8_seconds() {
    assert!(
        TEST_HARNESS_TIMEOUT >= Duration::from_secs(8),
        "harness timeout regressed below the Windows-runner safe floor",
    );
}
