//! Regression tests for round-24 finding: select-with-cancellation leaks
//! stale wakers and inflates `waiting_receivers` on sibling channels.
//!
//! Background: when `channel.select` parks a task on multiple channels,
//! the scheduler registers a waker on each channel's `recv_wakers` /
//! `send_wakers` queue. Each `register_recv_waker` increments
//! `waiting_receivers` unconditionally. When ONE channel resolves the
//! select, the wakers on the OTHER channels remain in their queues
//! forever — and their `waiting_receivers` counters stay incremented.
//!
//! Observable consequence: a later, unrelated sender on a rendezvous
//! sibling channel sees `waiting_receivers > 0` in `try_send`, places
//! its value in the handoff slot, and returns `Sent` even though no
//! real receiver is waiting. The rendezvous handshake is broken.
//!
//! Fix: `register_recv_waker` / `register_send_waker` now return a
//! `WakerId`; `Channel::remove_recv_waker` / `remove_send_waker`
//! deregister an entry by id, decrementing the counter as needed.
//! The scheduler's select arm uses these to clean up sibling wakers
//! after one branch fires.

use silt::value::{Channel, TryReceiveResult, TrySendResult, Value, WakerId};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Unit test: emulate the scheduler's pre-fix select behavior using
/// only the Channel API. Without `remove_recv_waker`, channel B's
/// `waiting_receivers` would stay incremented at 1 after channel A
/// resolves the select. With the fix, the test calls
/// `remove_recv_waker(b_id)` to deregister B's waker, returning
/// the counter to 0.
///
/// To make this test load-bearing for the bug (i.e. fail before the
/// `remove_recv_waker` API existed), it also asserts
/// `recv_waker_queue_len() == 0` — the waker closure itself must be
/// gone, not just the counter.
#[test]
fn test_select_sibling_waker_removed_clears_counter_and_queue() {
    let a = Arc::new(Channel::new(1, 0)); // rendezvous
    let b = Arc::new(Channel::new(2, 0)); // rendezvous

    // Simulate the scheduler's select-arm registration on both
    // channels. The registration intentionally returns a WakerId so
    // a sibling-cleanup pass can remove the loser.
    let fired = Arc::new(AtomicUsize::new(0));

    let fired_a = fired.clone();
    let a_id = a.register_recv_waker(Box::new(move || {
        fired_a.fetch_add(1, Ordering::SeqCst);
    }));
    let fired_b = fired.clone();
    let b_id = b.register_recv_waker(Box::new(move || {
        fired_b.fetch_add(1, Ordering::SeqCst);
    }));

    assert_eq!(a.waiting_receivers_count(), 1);
    assert_eq!(b.waiting_receivers_count(), 1);
    assert_eq!(a.recv_waker_queue_len(), 1);
    assert_eq!(b.recv_waker_queue_len(), 1);

    // Channel A "fires" — a sender on A places a value, which pops
    // and runs A's waker. wake_recv decrements A's counter.
    assert!(matches!(a.try_send(Value::Int(7)), TrySendResult::Sent));
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    assert_eq!(a.waiting_receivers_count(), 0);
    assert_eq!(a.recv_waker_queue_len(), 0);

    // Sibling cleanup: scheduler deregisters B's waker since the
    // select resolved via A. Without this step, B's counter stays at
    // 1 and B's queue still holds the stale waker.
    let removed = b.remove_recv_waker(b_id);
    assert!(
        removed,
        "remove_recv_waker should find the registered waker"
    );

    assert_eq!(
        b.waiting_receivers_count(),
        0,
        "B's waiting_receivers counter must return to 0 after sibling cleanup"
    );
    assert_eq!(
        b.recv_waker_queue_len(),
        0,
        "B's recv_wakers queue must be empty after sibling cleanup"
    );

    // a_id is referenced (silences unused-binding warning) but A's
    // waker has already been popped and consumed, so removal returns
    // false — that's the correct, idempotent behavior.
    assert!(!a.remove_recv_waker(a_id));
}

/// Without the deregistration API the counter stays > 0; rendezvous
/// `try_send` then sees a phantom receiver, places the value into the
/// handoff slot, and returns `Sent` with NO real receiver waiting on
/// the channel. This test demonstrates the broken contract WAS the
/// bug and that the fix restores correct rendezvous semantics: with
/// the sibling waker properly deregistered, `try_send` sees zero
/// waiting receivers and returns `Full`.
#[test]
fn test_after_select_cleanup_rendezvous_send_blocks_with_no_receiver() {
    let a = Arc::new(Channel::new(10, 0));
    let b = Arc::new(Channel::new(11, 0));

    // Stage a select that will resolve via A.
    let a_id = a.register_recv_waker(Box::new(|| {}));
    let b_id = b.register_recv_waker(Box::new(|| {}));

    // A fires — pops its waker and decrements A's counter.
    assert!(matches!(a.try_send(Value::Int(1)), TrySendResult::Sent));
    // Drain A's value so the channel is fully reset for the assertion.
    assert!(matches!(a.try_receive(), TryReceiveResult::Value(_)));

    // Scheduler-style sibling cleanup on B (the loser).
    assert!(b.remove_recv_waker(b_id));
    // a_id retained for symmetry; A's waker already drained.
    let _ = a_id;

    // CRITICAL CONTRACT: with B's stale waker removed and counter
    // back to 0, a fresh `try_send` on rendezvous channel B must
    // return `Full` (no receiver waiting). Before the fix, the
    // counter was still 1 and `try_send` would falsely report
    // `Sent`, dropping the value into the handoff slot with no
    // counterparty.
    match b.try_send(Value::Int(99)) {
        TrySendResult::Full => {} // correct: no real receiver
        TrySendResult::Sent => panic!(
            "rendezvous try_send returned Sent with no receiver — \
             stale select-sibling waker leaked into waiting_receivers"
        ),
        TrySendResult::Closed => panic!("channel unexpectedly closed"),
    }

    // Sanity: B's queue and counter are both zero.
    assert_eq!(b.waiting_receivers_count(), 0);
    assert_eq!(b.recv_waker_queue_len(), 0);
}

/// remove_send_waker for the buffered-send arm of select. Verifies
/// the symmetric API exists and works for send wakers (no counter on
/// send side, but the queue must still be drained to avoid leaking
/// closures and capturing Arc references).
#[test]
fn test_remove_send_waker_drains_queue_entry() {
    let ch = Arc::new(Channel::new(20, 1)); // capacity 1 buffered

    // Fill the buffer so the next try_send returns Full.
    assert!(matches!(ch.try_send(Value::Int(1)), TrySendResult::Sent));

    let fired = Arc::new(AtomicUsize::new(0));
    let fired_clone = fired.clone();
    let wid = ch.register_send_waker(Box::new(move || {
        fired_clone.fetch_add(1, Ordering::SeqCst);
    }));

    assert_eq!(ch.send_waker_queue_len(), 1);

    // Sibling cleanup removes the waker before any send-space opens up.
    assert!(ch.remove_send_waker(wid));
    assert_eq!(ch.send_waker_queue_len(), 0);

    // The waker did NOT fire — cleanup must not invoke the closure.
    assert_eq!(fired.load(Ordering::SeqCst), 0);

    // remove_send_waker on a non-existent id is a no-op.
    assert!(!ch.remove_send_waker(WakerId(999_999)));
}

/// Tight-loop simulation of repeated select-then-cleanup. Models a
/// scheduler that issues many selects across the same channel pair.
/// Without the cleanup API, every select would leave one stale waker
/// on the loser channel and the counter would grow without bound.
/// With the API in place, the counters stay at zero throughout.
#[test]
fn test_repeated_select_cleanup_keeps_counters_bounded() {
    let a = Arc::new(Channel::new(30, 0));
    let b = Arc::new(Channel::new(31, 0));

    for _ in 0..1000 {
        let a_id = a.register_recv_waker(Box::new(|| {}));
        let b_id = b.register_recv_waker(Box::new(|| {}));

        // A wins.
        assert!(matches!(a.try_send(Value::Int(0)), TrySendResult::Sent));
        assert!(matches!(a.try_receive(), TryReceiveResult::Value(_)));

        // Sibling cleanup on B (loser); A's already drained.
        let _ = a.remove_recv_waker(a_id);
        assert!(b.remove_recv_waker(b_id));
    }

    assert_eq!(a.waiting_receivers_count(), 0);
    assert_eq!(b.waiting_receivers_count(), 0);
    assert_eq!(a.recv_waker_queue_len(), 0);
    assert_eq!(b.recv_waker_queue_len(), 0);
}

/// End-to-end test through the silt binary: spawns a task that
/// performs `channel.select([a, b])` resolving via A, then attempts
/// a non-blocking try-send on B (modeled as a select with a default
/// branch on receive). Without the fix, the scheduler leaks B's
/// waker and a later rendezvous send into a sibling phantom-receives.
///
/// We assert the program completes with the expected output within
/// a reasonable wall-clock window — a leak that broke rendezvous
/// would either drop messages or hang.
#[test]
fn test_select_then_send_on_sibling_does_not_phantom_receive() {
    use std::path::PathBuf;
    use std::process::Command;

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

    // Drive a silt program: receiver task selects over (a, b) on
    // rendezvous channels, A is fed first and resolves the select.
    // Then main sends on B; with the bug, B's `waiting_receivers`
    // would be 1 (leaked from select), so try_send-on-rendezvous
    // would falsely succeed AND no real receiver would consume the
    // value. The program then waits for a real receive on B —
    // which would hang forever (or block until SILT_IO_TIMEOUT, or
    // deadlock detection fires).
    //
    // With the fix, the leaked counter is gone; main's send blocks
    // until the second receiver task picks up B, and the program
    // completes cleanly.
    let src = r#"
import channel
import task
import time

fn main() {
  let a = channel.new(0)
  let b = channel.new(0)

  -- Receiver 1 selects over a and b; expects to resolve via a.
  let r1 = task.spawn { ->
    match channel.select([a, b]) {
      (_, Message(_)) -> "got_one"
      _ -> "other"
    }
  }
  -- Give r1 time to register both wakers.
  time.sleep(time.ms(50))
  -- Resolve r1 via a. With the bug, b's waiting_receivers stays
  -- at 1 forever even after r1 completes.
  channel.send(a, 1)
  let v1 = task.join(r1)

  -- Receiver 2: real consumer of b.
  let r2 = task.spawn { -> channel.receive(b) }
  -- Give r2 time to park.
  time.sleep(time.ms(50))
  -- Send into b. If the leaked select-waker still inflates b's
  -- waiting_receivers, this might falsely succeed without r2 ever
  -- waking. With the fix, this blocks until r2 is the actual receiver.
  channel.send(b, 2)
  let v2 = task.join(r2)

  match v2 {
    Message(_) -> println("ok:{v1}")
    _ -> println("fail:{v1}")
  }
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "silt_swc_{}_{}.silt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&tmp, src).unwrap();
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
        elapsed < Duration::from_secs(10),
        "select-cleanup test took too long ({elapsed:?}); a leaked \
         select waker likely caused a hang. stdout={stdout:?} \
         stderr={stderr:?}"
    );
    assert!(
        stdout.contains("ok:"),
        "expected ok output; got stdout={stdout:?} stderr={stderr:?}"
    );
}
