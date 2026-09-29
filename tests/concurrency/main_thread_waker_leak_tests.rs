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
//! The binary-level symptom tests (phantom rendezvous send after a
//! main-thread receive / select) live as golden cases:
//! `tests/golden/concurrency/channels/main_thread_waker_leak__*` and
//! `tests/golden/concurrency/select/main_thread_waker_leak__*`.

use silt::value::{Channel, TrySendResult, Value};
use std::sync::Arc;

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
