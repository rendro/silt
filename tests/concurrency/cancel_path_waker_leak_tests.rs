//! Regression tests for round-27 findings B1–B4: the scheduler's
//! `BlockReason::Receive` / `BlockReason::Send` cancel-cleanup path
//! takes the task slot but does NOT deregister the waker that the
//! BlockReason arm just registered on the channel. The `WakerId`
//! returned by `register_recv_waker` / `register_send_waker` is
//! silently discarded at the call site, leaving a dead waker and an
//! inflated `waiting_receivers` / `waiting_senders` counter on the
//! channel.
//!
//! Observable consequences (all four reproduced by the golden cases
//! `tests/golden/concurrency/cancel/cancel_path_waker_leak__b{1,2,3,4}_*`):
//!
//! - **B1** phantom rendezvous send after cancel-during-receive:
//!   cancelled receiver's waker stays in `recv_wakers` with
//!   `waiting_receivers > 0`, so a later unrelated `try_send` drops
//!   the value into the handoff slot and returns `Sent` with no real
//!   receiver.
//! - **B2** receiver starvation: dead waker at the FIFO head of
//!   `recv_wakers` shadows a real receiver; `wake_recv` pops the
//!   dead entry (no-op) and the real receiver never wakes →
//!   deadlock detector fires.
//! - **B3** sender starvation (rendezvous) — symmetric to B2 on
//!   `send_wakers`.
//! - **B4** sender starvation (buffered, cap=1) — symmetric to B3
//!   on a buffered channel after drain.
//!
//! Fix: `src/value.rs` introduces a `WakerRegistration` RAII guard
//! that owns `(Arc<Channel>, WakerId, WakerKind)` and calls
//! `remove_recv_waker` / `remove_send_waker` on drop. The scheduler's
//! Receive/Send/Select arms and the main-thread helpers in
//! `src/builtins/concurrency.rs` now own guards instead of raw
//! `WakerId`s, so the cancel path closes automatically when the
//! owning closure / Vec is dropped.

// ── Rust-level unit tests exercising the guard directly ─────────────
//
// These pin the `WakerRegistration` RAII semantics at the Channel API
// layer without driving the VM. They would fail immediately if the
// guard's `Drop` regressed (no-op impl, misrouted kind, etc).

use silt::value::{Channel, TryReceiveResult, TrySendResult, Value, WakerKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A freshly-dropped recv guard deregisters its waker: counter back to 0,
/// queue empty, and a subsequent rendezvous `try_send` correctly
/// reports no receiver.
#[test]
fn test_recv_waker_registration_guard_deregisters_on_drop() {
    let ch = Arc::new(Channel::new(300, 0));
    assert_eq!(ch.waiting_receivers_count(), 0);

    {
        let _reg = ch.register_recv_waker_guard(Box::new(|| {}));
        assert_eq!(ch.waiting_receivers_count(), 1);
        assert_eq!(ch.recv_waker_queue_len(), 1);
    } // guard drops here

    assert_eq!(
        ch.waiting_receivers_count(),
        0,
        "guard Drop must deregister the waker"
    );
    assert_eq!(ch.recv_waker_queue_len(), 0);
    // Phantom-send probe: with counter at 0, try_send must return Full.
    assert!(matches!(ch.try_send(Value::Int(1)), TrySendResult::Full));
}

/// Symmetric test for send-side guards.
#[test]
fn test_send_waker_registration_guard_deregisters_on_drop() {
    let ch = Arc::new(Channel::new(301, 0));
    assert_eq!(ch.send_waker_queue_len(), 0);

    {
        let _reg = ch.register_send_waker_guard(Box::new(|| {}));
        assert_eq!(ch.send_waker_queue_len(), 1);
    }

    assert_eq!(
        ch.send_waker_queue_len(),
        0,
        "guard Drop must deregister the send waker"
    );
}

/// If the waker fires (wake_recv pops it), the guard's Drop is a
/// harmless no-op — `remove_recv_waker` returns false without
/// touching the counter (wake_recv already decremented it).
#[test]
fn test_recv_guard_drop_after_fire_is_idempotent() {
    let ch = Arc::new(Channel::new(302, 0));
    let fired = Arc::new(AtomicBool::new(false));
    let fired_clone = fired.clone();

    let reg = ch.register_recv_waker_guard(Box::new(move || {
        fired_clone.store(true, Ordering::SeqCst);
    }));
    assert_eq!(reg.kind(), WakerKind::Recv);
    assert_eq!(ch.waiting_receivers_count(), 1);

    // Simulate a sender arriving: try_send on a rendezvous channel
    // with a waiting receiver pops + fires the waker.
    assert!(matches!(ch.try_send(Value::Int(42)), TrySendResult::Sent));
    assert!(fired.load(Ordering::SeqCst), "waker should have fired");
    assert_eq!(
        ch.waiting_receivers_count(),
        0,
        "wake_recv decrements counter on pop"
    );

    // Drop the guard; counter must stay at 0, not underflow / inflate.
    drop(reg);
    assert_eq!(ch.waiting_receivers_count(), 0);
    assert_eq!(ch.recv_waker_queue_len(), 0);

    // Drain the value to leave channel state clean.
    assert!(matches!(ch.try_receive(), TryReceiveResult::Value(_)));
}

/// Simulate the scheduler select-arm: register guards on two
/// rendezvous channels, channel A fires, dropping the Vec of guards
/// deregisters B's (still-pending) waker.
#[test]
fn test_select_vec_of_guards_cleans_up_losing_sibling() {
    let a = Arc::new(Channel::new(400, 0));
    let b = Arc::new(Channel::new(401, 0));

    let mut entries = Vec::new();
    entries.push(a.register_recv_waker_guard(Box::new(|| {})));
    entries.push(b.register_recv_waker_guard(Box::new(|| {})));

    assert_eq!(a.waiting_receivers_count(), 1);
    assert_eq!(b.waiting_receivers_count(), 1);

    // Channel A fires — its waker is popped by try_send + wake_recv.
    assert!(matches!(a.try_send(Value::Int(5)), TrySendResult::Sent));
    assert!(matches!(a.try_receive(), TryReceiveResult::Value(_)));
    assert_eq!(a.waiting_receivers_count(), 0);
    assert_eq!(b.waiting_receivers_count(), 1); // B still inflated

    // Drop all guards — B's waker gets deregistered.
    drop(entries);

    assert_eq!(a.waiting_receivers_count(), 0);
    assert_eq!(b.waiting_receivers_count(), 0);
    assert_eq!(b.recv_waker_queue_len(), 0);
    // Probe: try_send on B must now observe no receiver.
    assert!(matches!(b.try_send(Value::Int(9)), TrySendResult::Full));
}
