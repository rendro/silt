//! Regression tests for LATENT V1 (round-65 follow-up): when a task
//! blocked on `task.join(h)` or on I/O completion is cancelled, the
//! generic cancel cleanup at `src/scheduler.rs:669` drains
//! `task_slot` (making the waker closure inert when fired) but does
//! NOT deregister the closure from the joinee's
//! `TaskHandle::join_wakers` Vec or from `IoCompletion::wakers`. The
//! closure stays in the Vec until the joinee/io completes, holding
//! `Arc<Mutex<Option<Task>>>` plus `Arc<SchedulerInner>`.
//!
//! Symptom: N cancelled `task.join(h)` blocks on a long-running joinee
//! leak N closures into `join_wakers`. Each closure pins one
//! `Arc<SchedulerInner>` and the (already-emptied) task slot.
//!
//! Fix: `src/value.rs` introduces `JoinWakerRegistration` and
//! `IoWakerRegistration` RAII guards (analogous to
//! `WakerRegistration` for channels). The scheduler's Join and Io
//! arms use the new `register_join_waker_guard` /
//! `register_waker_guard` constructors and store the guard inside
//! the per-arm `cancel_cleanup` closure, so the guard's Drop runs on
//! both the cancel path and the normal-wake path.
//!
//! These Rust-level unit tests pin the RAII semantics at the
//! TaskHandle / IoCompletion API layer without driving the VM. They
//! would fail immediately if the guard's Drop regressed (no-op impl,
//! wrong id matching, missing `remove_*` call, etc).
//!
//! Run with:
//! `cargo test --features test-hooks --test cancel_path_join_io_waker_leak_tests`
//!
//! The whole module is gated on `feature = "test-hooks"` because the
//! accessors that count waker entries (`TaskHandle::join_waker_count`,
//! `IoCompletion::waker_count`) are only compiled under that feature
//! (or `cfg(test)` of the lib crate) — they are test-only
//! introspection, not part of the public runtime API.

#![cfg(feature = "test-hooks")]

use silt::value::{IoCompletion, TaskHandle, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ── TaskHandle::register_join_waker_guard ───────────────────────────

/// A freshly-dropped join-waker guard deregisters its entry: the
/// joinee's `join_wakers` count returns to 0 even though the joinee
/// is still running.
#[test]
fn join_waker_guard_drop_deregisters_entry() {
    let handle = Arc::new(TaskHandle::new(1));
    assert_eq!(handle.join_waker_count(), 0);

    {
        let _reg = handle.register_join_waker_guard(Box::new(|| {}));
        assert_eq!(handle.join_waker_count(), 1);
    } // guard drops here

    assert_eq!(
        handle.join_waker_count(),
        0,
        "guard Drop must deregister the join waker"
    );
}

/// N cancelled `task.join(h)` blocks on a long-running joinee must
/// not leak waker closures into `join_wakers`. Mirrors the leak
/// scenario described in LATENT V1: each `_reg` here represents one
/// parked-then-cancelled joiner; dropping the cancel cleanup (which
/// owns the guard) must remove the entry.
#[test]
fn cancelled_join_waker_is_deregistered_from_joinee_join_wakers() {
    let joinee = Arc::new(TaskHandle::new(42));
    assert_eq!(joinee.join_waker_count(), 0);

    // Register N guards, simulating N tasks parked on `task.join(joinee)`.
    let n = 50;
    let mut guards = Vec::with_capacity(n);
    for _ in 0..n {
        guards.push(joinee.register_join_waker_guard(Box::new(|| {
            // In the real scheduler arm this closure does
            // `slot.lock().take().map(|t| requeue(...))`; for the
            // RAII test we just need the closure to exist and be
            // owned by the guard.
        })));
    }
    assert_eq!(joinee.join_waker_count(), n);

    // Simulate cancellation: each task's per-arm cancel cleanup is
    // dropped (which moves the guard into the closure scope and
    // drops it). Drop them all.
    drop(guards);

    assert_eq!(
        joinee.join_waker_count(),
        0,
        "all cancelled-joiner waker entries must be deregistered \
         (LATENT V1: closures previously stayed in the Vec until the \
         joinee finally completed)"
    );
}

/// If the joinee completes BEFORE the guard is dropped, the closure
/// is drained by `complete()` and the guard's Drop is a harmless
/// no-op (the entry is already gone).
#[test]
fn join_guard_drop_after_complete_is_idempotent() {
    let handle = Arc::new(TaskHandle::new(2));
    let fired = Arc::new(AtomicBool::new(false));
    let fired_clone = fired.clone();

    let reg = handle.register_join_waker_guard(Box::new(move || {
        fired_clone.store(true, Ordering::SeqCst);
    }));
    assert_eq!(handle.join_waker_count(), 1);

    // Joinee completes — drains all join_wakers, fires each closure.
    handle.complete(Ok(Value::Int(7)));
    assert!(fired.load(Ordering::SeqCst), "waker should have fired");
    assert_eq!(handle.join_waker_count(), 0);

    // Drop the guard; count stays at 0, no panic / underflow.
    drop(reg);
    assert_eq!(handle.join_waker_count(), 0);
}

/// If the joinee is already complete at registration time,
/// `register_join_waker_guard` fires the waker inline and stores no
/// entry; the returned guard's Drop is a no-op.
#[test]
fn join_guard_inline_fire_when_already_complete() {
    let handle = Arc::new(TaskHandle::new(3));
    handle.complete(Ok(Value::Int(99)));

    let fired = Arc::new(AtomicBool::new(false));
    let fired_clone = fired.clone();

    let reg = handle.register_join_waker_guard(Box::new(move || {
        fired_clone.store(true, Ordering::SeqCst);
    }));
    assert!(
        fired.load(Ordering::SeqCst),
        "waker must fire inline when handle is already complete"
    );
    assert_eq!(
        handle.join_waker_count(),
        0,
        "no entry stored when handle is already complete"
    );

    drop(reg); // no-op deregister
    assert_eq!(handle.join_waker_count(), 0);
}

/// Two registrations get distinct ids; dropping one removes only
/// that entry, not the other.
#[test]
fn join_waker_guards_have_distinct_ids() {
    let handle = Arc::new(TaskHandle::new(4));
    let r1 = handle.register_join_waker_guard(Box::new(|| {}));
    let r2 = handle.register_join_waker_guard(Box::new(|| {}));
    assert_ne!(r1.id(), r2.id());
    assert_eq!(handle.join_waker_count(), 2);

    drop(r1);
    assert_eq!(
        handle.join_waker_count(),
        1,
        "dropping r1 removes only its entry, not r2's"
    );

    drop(r2);
    assert_eq!(handle.join_waker_count(), 0);
}

// ── IoCompletion::register_waker_guard ──────────────────────────────

/// A freshly-dropped io-waker guard deregisters its entry: the
/// completion's `wakers` count returns to 0 even though the I/O has
/// not produced a value yet.
#[test]
fn io_waker_guard_drop_deregisters_entry() {
    let completion = IoCompletion::new();
    assert_eq!(completion.waker_count(), 0);

    {
        let _reg = completion.register_waker_guard(Box::new(|| {}));
        assert_eq!(completion.waker_count(), 1);
    }

    assert_eq!(
        completion.waker_count(),
        0,
        "guard Drop must deregister the io waker"
    );
}

/// N cancelled I/O-blocked tasks must not leak waker closures into
/// `IoCompletion::wakers`. Mirrors the leak scenario described in
/// LATENT V1.
#[test]
fn cancelled_io_waker_is_deregistered_from_io_completion_wakers() {
    let completion = IoCompletion::new();
    assert_eq!(completion.waker_count(), 0);

    let n = 50;
    let mut guards = Vec::with_capacity(n);
    for _ in 0..n {
        guards.push(completion.register_waker_guard(Box::new(|| {})));
    }
    assert_eq!(completion.waker_count(), n);

    // Simulate cancellation: drop each per-arm cancel cleanup
    // (which owns the guard).
    drop(guards);

    assert_eq!(
        completion.waker_count(),
        0,
        "all cancelled-io-waiter waker entries must be deregistered \
         (LATENT V1: closures previously stayed in the Vec until the \
         I/O finally produced a value)"
    );
}

/// If the I/O completes BEFORE the guard is dropped, the closure is
/// drained by `complete()` and the guard's Drop is a harmless no-op.
#[test]
fn io_guard_drop_after_complete_is_idempotent() {
    let completion = IoCompletion::new();
    let fired = Arc::new(AtomicBool::new(false));
    let fired_clone = fired.clone();

    let reg = completion.register_waker_guard(Box::new(move || {
        fired_clone.store(true, Ordering::SeqCst);
    }));
    assert_eq!(completion.waker_count(), 1);

    // I/O completes — drains all wakers, fires each closure.
    assert!(completion.complete(Value::Int(123)));
    assert!(fired.load(Ordering::SeqCst), "waker should have fired");
    assert_eq!(completion.waker_count(), 0);

    drop(reg);
    assert_eq!(completion.waker_count(), 0);
}

/// If the completion is already finished at registration time, the
/// waker fires inline and no entry is stored; the guard's Drop is a
/// no-op.
#[test]
fn io_guard_inline_fire_when_already_complete() {
    let completion = IoCompletion::new();
    assert!(completion.complete(Value::Int(456)));

    let fired = Arc::new(AtomicBool::new(false));
    let fired_clone = fired.clone();

    let reg = completion.register_waker_guard(Box::new(move || {
        fired_clone.store(true, Ordering::SeqCst);
    }));
    assert!(
        fired.load(Ordering::SeqCst),
        "waker must fire inline when completion is already finished"
    );
    assert_eq!(
        completion.waker_count(),
        0,
        "no entry stored when completion is already finished"
    );

    drop(reg);
    assert_eq!(completion.waker_count(), 0);
}

/// Two registrations get distinct ids; dropping one removes only
/// that entry.
#[test]
fn io_waker_guards_have_distinct_ids() {
    let completion = IoCompletion::new();
    let r1 = completion.register_waker_guard(Box::new(|| {}));
    let r2 = completion.register_waker_guard(Box::new(|| {}));
    assert_ne!(r1.id(), r2.id());
    assert_eq!(completion.waker_count(), 2);

    drop(r1);
    assert_eq!(completion.waker_count(), 1);

    drop(r2);
    assert_eq!(completion.waker_count(), 0);
}

// ── Coexistence with the legacy non-guard API ───────────────────────

/// `register_join_waker` (no guard) still functions for the
/// non-cancellation call site in `concurrency::main_thread_wait_for_join`.
/// It pushes an entry that is drained by `complete()`.
#[test]
fn legacy_register_join_waker_still_works() {
    let handle = Arc::new(TaskHandle::new(5));
    let fired = Arc::new(AtomicBool::new(false));
    let fired_clone = fired.clone();

    handle.register_join_waker(Box::new(move || {
        fired_clone.store(true, Ordering::SeqCst);
    }));
    assert_eq!(handle.join_waker_count(), 1);

    handle.complete(Ok(Value::Unit));
    assert!(fired.load(Ordering::SeqCst));
    assert_eq!(handle.join_waker_count(), 0);
}

/// `register_waker` (no guard) still functions for legacy I/O call
/// sites. It pushes an entry that is drained by `complete()`.
#[test]
fn legacy_register_io_waker_still_works() {
    let completion = IoCompletion::new();
    let fired = Arc::new(AtomicBool::new(false));
    let fired_clone = fired.clone();

    completion.register_waker(Box::new(move || {
        fired_clone.store(true, Ordering::SeqCst);
    }));
    assert_eq!(completion.waker_count(), 1);

    assert!(completion.complete(Value::Unit));
    assert!(fired.load(Ordering::SeqCst));
    assert_eq!(completion.waker_count(), 0);
}
