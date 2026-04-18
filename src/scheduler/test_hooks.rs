//! Deterministic event-injection hooks for the M:N scheduler.
//!
//! The scheduler's correctness depends on the precise interleaving of
//! several non-atomic transitions (counter bumps, queue pushes,
//! waker registrations). The historical fan-in / dequeue-to-register
//! races could only be reproduced probabilistically — most trials raced
//! through cleanly, a small fraction tripped the false-positive
//! detector. This module exposes per-thread instrumentation callbacks
//! at every transition point that matters for those races, so a Phase-3
//! regression test can install a hook that pauses (sleeps, blocks on a
//! barrier, releases a oneshot) at exactly the racy moment, turning a
//! 1-in-100 flake into a deterministic 1-of-1 failure.
//!
//! ## Hook taxonomy
//!
//! | Hook        | Fired when                                                  |
//! |-------------|-------------------------------------------------------------|
//! | `on_submit` | After `Scheduler::submit` bumps both counters, before push  |
//! | `on_dequeue`| Inside `worker_loop`, immediately after `pop_front`         |
//! | `on_park`   | Inside the `Blocked` arm, just before `register_*_waker_*`  |
//! | `on_wake`   | Inside `requeue`, before any counter mutation               |
//!
//! Each fire site passes a static `&'static str` tag identifying the
//! exact transition (e.g. `"blocked_arm_entry_recv"`) so a hook can
//! act selectively. A hook that returns immediately is a no-op; a hook
//! that sleeps or blocks on a synchronization primitive forces a
//! deterministic schedule.
//!
//! ## Storage shape
//!
//! Each hook lives in its own thread-local `RefCell<Option<Hook>>`. A
//! thread-local is the right shape for these hooks because every
//! transition fires on a known thread — the worker thread for
//! `on_dequeue`/`on_park`/`on_wake`, and the caller thread for
//! `on_submit`. Tests that want a hook to fire only on a specific
//! thread install it on that thread; tests that want global behavior
//! install it on every thread that may participate (typically each
//! worker plus the main thread).
//!
//! ## Lifetime
//!
//! Hooks are `Box<dyn Fn(&'static str) + Send + 'static>` — owned by
//! the thread-local cell and dropped when the cell is overwritten or
//! the thread exits. Hooks may be invoked many times (once per fire
//! site per execution); they MUST be `Fn`, not `FnOnce`.
//!
//! ## Concurrency
//!
//! Each hook runs on the thread that fired it, so the hook body itself
//! has no race with other hook bodies. A hook that reads or writes
//! shared state is responsible for its own synchronization (typically
//! an `Arc<Mutex<...>>` captured in the closure).

use std::cell::RefCell;

/// Concrete signature for a scheduler-event hook. Captures whatever
/// state the test needs (counters, barriers, oneshots) and inspects the
/// `tag` to decide what to do at each fire site.
pub type Hook = Box<dyn Fn(&'static str) + Send + 'static>;

thread_local! {
    static ON_SUBMIT_HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    static ON_DEQUEUE_HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    static ON_PARK_HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    static ON_WAKE_HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

// ── Install / clear API ───────────────────────────────────────────

/// Install a hook to fire after `Scheduler::submit` bumps both
/// counters, before the queue push. Returns the previously-installed
/// hook (if any) so the caller can restore it on tear-down.
pub fn install_on_submit(hook: Hook) -> Option<Hook> {
    ON_SUBMIT_HOOK.with(|cell| cell.borrow_mut().replace(hook))
}

/// Install a hook to fire after a worker pops a task off the run
/// queue, before that task's slice executes. Returns the previously-
/// installed hook (if any).
pub fn install_on_dequeue(hook: Hook) -> Option<Hook> {
    ON_DEQUEUE_HOOK.with(|cell| cell.borrow_mut().replace(hook))
}

/// Install a hook to fire as the `Blocked` arm enters the per-reason
/// register-waker block, BEFORE `register_*_waker_guard`. The tag
/// distinguishes recv / send / select / join / io. Returns the
/// previously-installed hook (if any).
pub fn install_on_park(hook: Hook) -> Option<Hook> {
    ON_PARK_HOOK.with(|cell| cell.borrow_mut().replace(hook))
}

/// Install a hook to fire as `requeue` runs, before any counter
/// mutation. Returns the previously-installed hook (if any).
pub fn install_on_wake(hook: Hook) -> Option<Hook> {
    ON_WAKE_HOOK.with(|cell| cell.borrow_mut().replace(hook))
}

/// Clear every hook on the calling thread. Test tear-down convenience
/// so a panicking test cannot leak a hook into the next test (which
/// would run on the same OS thread under `cargo test`).
pub fn clear_all() {
    ON_SUBMIT_HOOK.with(|cell| cell.borrow_mut().take());
    ON_DEQUEUE_HOOK.with(|cell| cell.borrow_mut().take());
    ON_PARK_HOOK.with(|cell| cell.borrow_mut().take());
    ON_WAKE_HOOK.with(|cell| cell.borrow_mut().take());
}

// ── Fire entry points (called by the `fire_hook!` macro) ──────────
//
// Each entry point is a free function rather than a closure capture
// because the macro at the call site has no access to the
// `thread_local!`'s `with` plumbing. Bodies are deliberately tiny so
// the no-hook fast path is a single thread-local read + None check
// (no allocation, no extra indirection).

/// Fired by `Scheduler::submit`.
#[inline]
pub fn on_submit(tag: &'static str) {
    ON_SUBMIT_HOOK.with(|cell| {
        if let Some(ref hook) = *cell.borrow() {
            hook(tag);
        }
    });
}

/// Fired immediately after `pop_front` inside `worker_loop`.
#[inline]
pub fn on_dequeue(tag: &'static str) {
    ON_DEQUEUE_HOOK.with(|cell| {
        if let Some(ref hook) = *cell.borrow() {
            hook(tag);
        }
    });
}

/// Fired at the entry of each `BlockReason` arm, before the per-arm
/// `register_*_waker_guard` call.
#[inline]
pub fn on_park(tag: &'static str) {
    ON_PARK_HOOK.with(|cell| {
        if let Some(ref hook) = *cell.borrow() {
            hook(tag);
        }
    });
}

/// Fired at the start of `requeue`, before any counter mutation.
#[inline]
pub fn on_wake(tag: &'static str) {
    ON_WAKE_HOOK.with(|cell| {
        if let Some(ref hook) = *cell.borrow() {
            hook(tag);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn no_hook_installed_is_noop() {
        clear_all();
        on_submit("anything");
        on_dequeue("anything");
        on_park("anything");
        on_wake("anything");
        // Reaching here without panic means the no-op fast path works.
    }

    #[test]
    fn install_on_submit_observed() {
        clear_all();
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        install_on_submit(Box::new(move |_tag| {
            count2.fetch_add(1, Ordering::SeqCst);
        }));
        on_submit("submit_after_counters");
        on_submit("submit_after_counters");
        assert_eq!(count.load(Ordering::SeqCst), 2);
        clear_all();
        on_submit("submit_after_counters");
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "clear_all should detach the hook"
        );
    }

    #[test]
    fn install_returns_previous_hook() {
        clear_all();
        install_on_park(Box::new(|_| {}));
        let prev = install_on_park(Box::new(|_| {}));
        assert!(prev.is_some(), "second install should return prior hook");
        clear_all();
    }

    #[test]
    fn tag_routes_through_to_hook() {
        clear_all();
        let last_tag: Arc<parking_lot::Mutex<Option<&'static str>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let last_tag2 = last_tag.clone();
        install_on_dequeue(Box::new(move |tag| {
            *last_tag2.lock() = Some(tag);
        }));
        on_dequeue("pop_front");
        assert_eq!(*last_tag.lock(), Some("pop_front"));
        clear_all();
    }

    #[test]
    fn hooks_are_thread_local() {
        clear_all();
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        install_on_wake(Box::new(move |_| {
            count2.fetch_add(1, Ordering::SeqCst);
        }));
        // Hook installed on this thread fires here.
        on_wake("requeue_entry");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // Hook NOT installed on a sibling thread does not fire there.
        let count3 = count.clone();
        std::thread::spawn(move || {
            on_wake("requeue_entry"); // No hook on this thread.
            assert_eq!(count3.load(Ordering::SeqCst), 1);
        })
        .join()
        .unwrap();
        clear_all();
    }
}
