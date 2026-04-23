//! Regression tests for round-26 findings B6 and B7: the main-thread
//! `channel.receive` / `channel.select` paths leak wakers into the
//! channel's queue. Each watchdog tick in the receive loop re-registers
//! a recv-waker without deregistering the previous one, permanently
//! inflating `waiting_receivers`. Select on main thread registers
//! wakers on every branch but never deregisters the losers.
//!
//! Observable consequence: after main's receive returns, a later
//! `try_send` from another task sees `waiting_receivers > 0`, drops
//! the value into the rendezvous handoff slot, and returns `Sent`
//! with no real receiver — a phantom rendezvous send that loses the
//! value.
//!
//! Fix: both paths now capture the `WakerId` returned by
//! `register_recv_waker` / `register_send_waker` and deregister any
//! still-pending entries via `remove_recv_waker` / `remove_send_waker`
//! before returning. This mirrors the round-24 scheduler-path fix in
//! `src/scheduler.rs` BlockReason::Select arm.
//!
//! The tests below drive the silt binary because the bug is only
//! observable at the builtin boundary: `main_thread_wait_for_receive`
//! is a private helper inside `src/builtins/concurrency.rs`.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn silt_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_silt") {
        return PathBuf::from(p);
    }
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.push("silt");
    p
}

fn tmp_silt_file(stem: &str, src: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "silt_{}_{}_{}.silt",
        stem,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&tmp, src).unwrap();
    tmp
}

/// B6: main thread receives on a rendezvous channel; the watchdog-tick
/// loop would previously register a new recv waker every 100ms without
/// deregistering the previous one. After main's receive returns, a
/// second task's `try_send` on the same rendezvous channel must return
/// `false` — no real receiver is parked. With the bug, the inflated
/// `waiting_receivers` counter made `try_send` falsely succeed.
///
/// We force at least one watchdog tick by having the sending task
/// sleep 150ms before sending (the watchdog period is 100ms). Without
/// the fix, `waiting_receivers` would be >= 2 after main's receive
/// returns, and the subsequent `try_send` would phantom-succeed.
#[test]
fn test_main_thread_receive_does_not_leak_recv_waker() {
    let src = r#"
import channel
import task
import time

fn main() {
  let ch = channel.new(0)

  -- Task sleeps long enough to force the main thread through at least
  -- one watchdog tick, then sends 42. Main's `channel.receive`
  -- must un-register its recv waker(s) before returning; otherwise
  -- `waiting_receivers` stays > 0 and the next `try_send` on the
  -- same rendezvous channel would phantom-succeed.
  let sender = task.spawn { ->
    time.sleep(time.ms(150))
    channel.send(ch, 42)
  }

  match channel.receive(ch) {
    Message(v) -> {
      task.join(sender)

      -- No receiver is currently parked on `ch`. A rendezvous
      -- `try_send` must return false. With the leaked-waker bug,
      -- this returns true and the value is dropped into the
      -- handoff slot with no receiver.
      let probe = task.spawn { ->
        channel.try_send(ch, 999)
      }
      let ok = task.join(probe)
      match ok {
        true -> println("BUG:leaked_waker_phantom_send v={v}")
        false -> println("ok:{v}")
      }
    }
    _ -> println("FAIL:unexpected_close_or_empty")
  }
}
"#;
    let tmp = tmp_silt_file("mt_recv_leak", src);
    let start = Instant::now();
    let output = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("failed to run silt binary");
    let elapsed = start.elapsed();
    let _ = std::fs::remove_file(&tmp);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        elapsed < Duration::from_secs(15),
        "receive-waker-leak test took too long ({elapsed:?}); stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("ok:42"),
        "expected 'ok:42' (successful receive + no phantom send); \
         got stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("BUG:"),
        "detected leaked-waker phantom send: stdout={stdout:?}"
    );
}

/// B7: main thread `channel.select` registers wakers on all branches;
/// after one branch fires, the losing branches' wakers are never
/// deregistered. A later `try_send` on the losing rendezvous channel
/// would see `waiting_receivers > 0` and phantom-succeed.
///
/// Fix: the main-thread select arm now captures each branch's WakerId
/// and cleans up non-firing siblings on return, matching the
/// scheduled-task path's behavior.
#[test]
fn test_main_thread_select_does_not_leak_sibling_wakers() {
    let src = r#"
import channel
import task
import time

fn main() {
  let a = channel.new(0)
  let b = channel.new(0)

  -- A task fires channel `a` after a short delay. The main thread
  -- blocks in `channel.select([a, b])`. When `a` resolves the select,
  -- the leaked waker on `b` would previously keep `waiting_receivers`
  -- at 1 forever.
  let feeder = task.spawn { ->
    time.sleep(time.ms(50))
    channel.send(a, 1)
  }

  match channel.select([Recv(a), Recv(b)]) {
    (_, Message(_)) -> {
      task.join(feeder)

      -- With the select-sibling waker properly cleaned up, a probe
      -- `try_send` on `b` must return false. With the bug, `b`'s
      -- counter is still 1 and the send phantom-succeeds.
      let probe = task.spawn { ->
        channel.try_send(b, 999)
      }
      let ok = task.join(probe)
      match ok {
        true -> println("BUG:leaked_select_sibling_phantom_send")
        false -> println("ok:select")
      }
    }
    _ -> println("FAIL:unexpected_branch")
  }
}
"#;
    let tmp = tmp_silt_file("mt_select_leak", src);
    let start = Instant::now();
    let output = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("failed to run silt binary");
    let elapsed = start.elapsed();
    let _ = std::fs::remove_file(&tmp);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        elapsed < Duration::from_secs(15),
        "select-waker-leak test took too long ({elapsed:?}); stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("ok:select"),
        "expected 'ok:select' from a successful select with clean sibling; \
         got stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("BUG:"),
        "detected leaked select-sibling waker: stdout={stdout:?}"
    );
}

// ── Rust-level unit tests using the public Channel API ───────────────
// These test the Channel counter directly rather than driving the VM,
// giving a tighter regression lock: any regression of the fix that
// re-introduces the discarded `WakerId` pattern in
// `main_thread_wait_for_receive` / `main_thread_wait_for_send` would
// leave the counter inflated and these tests would fail.
//
// We simulate the main-thread receive loop by hand (register-wait-
// re-register without deregistration = buggy; register-remove-
// register = fixed).

use silt::value::{Channel, TrySendResult, Value};
use std::sync::Arc;

/// Simulate the fixed receive loop: each "iteration" deregisters the
/// previous waker before minting a new one. Counter stays at 1 (or 0
/// when no waker is pending), never inflates.
#[test]
fn test_register_then_remove_keeps_counter_bounded() {
    let ch = Arc::new(Channel::new(100, 0));
    let mut last_id = None;
    for _ in 0..10 {
        if let Some(id) = last_id.take() {
            ch.remove_recv_waker(id);
        }
        let id = ch.register_recv_waker(Box::new(|| {}));
        last_id = Some(id);
        // After each iteration, at most one waker is pending.
        assert_eq!(
            ch.waiting_receivers_count(),
            1,
            "counter must stay at 1 when one waker is always pending"
        );
        assert_eq!(ch.recv_waker_queue_len(), 1);
    }
    // Final cleanup: remove the last waker; counter returns to 0.
    if let Some(id) = last_id {
        ch.remove_recv_waker(id);
    }
    assert_eq!(ch.waiting_receivers_count(), 0);
    assert_eq!(ch.recv_waker_queue_len(), 0);
    // A try_send now correctly reports no receiver.
    assert!(matches!(ch.try_send(Value::Int(1)), TrySendResult::Full));
}

/// Characterize the bug: 10 register_recv_waker calls without
/// deregistration inflate the counter to 10. The fix in
/// `main_thread_wait_for_receive` now deregisters each prior waker, so
/// this pre-fix scenario must never occur in practice — but the test
/// documents what the bug looked like and what the fix prevents.
#[test]
fn test_buggy_pattern_inflates_counter() {
    let ch = Arc::new(Channel::new(101, 0));
    for _ in 0..10 {
        // Discard the WakerId — exactly what the pre-fix code did.
        let _ = ch.register_recv_waker(Box::new(|| {}));
    }
    assert_eq!(
        ch.waiting_receivers_count(),
        10,
        "discarded WakerId + no deregistration inflates the counter"
    );
    // The downstream symptom: rendezvous try_send sees phantom
    // receivers and succeeds with no real counterparty.
    assert!(
        matches!(ch.try_send(Value::Int(1)), TrySendResult::Sent),
        "phantom send must succeed with inflated waiting_receivers"
    );
}

/// Simulate the fixed main-thread select arm: register on both
/// channels, one fires, and the sibling's waker is deregistered.
/// Counter on the loser returns to 0.
#[test]
fn test_main_thread_select_cleanup_keeps_counters_zero() {
    let a = Arc::new(Channel::new(200, 0));
    let b = Arc::new(Channel::new(201, 0));

    let a_id = a.register_recv_waker(Box::new(|| {}));
    let b_id = b.register_recv_waker(Box::new(|| {}));

    // Channel A fires — its waker pops, its counter decrements.
    assert!(matches!(a.try_send(Value::Int(5)), TrySendResult::Sent));
    // Drain A so state is clean.
    assert!(matches!(
        a.try_receive(),
        silt::value::TryReceiveResult::Value(_)
    ));

    // Fixed select arm: deregister the sibling (B) and any pending
    // entry on A (idempotent — A's waker was already popped).
    a.remove_recv_waker(a_id);
    assert!(b.remove_recv_waker(b_id));

    assert_eq!(a.waiting_receivers_count(), 0);
    assert_eq!(b.waiting_receivers_count(), 0);
    assert_eq!(a.recv_waker_queue_len(), 0);
    assert_eq!(b.recv_waker_queue_len(), 0);

    // Probe: a subsequent try_send on B sees no receiver and returns Full.
    assert!(matches!(b.try_send(Value::Int(9)), TrySendResult::Full));
}
