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
//! | `on_park`   | Inside the `Blocked` arm, just before `register_*_waker_*`  |
//!
//! Each fire site passes a static `&'static str` tag identifying the
//! exact transition (e.g. `"blocked_arm_entry_recv"`) so a hook can
//! act selectively. A hook that returns immediately is a no-op; a hook
//! that sleeps or blocks on a synchronization primitive forces a
//! deterministic schedule.
//!
//! ## Storage shape
//!
//! The `on_park` hook lives in a thread-local `RefCell<Option<Hook>>`. A
//! thread-local is the right shape because the transition fires on a
//! known thread — the worker thread for `on_park`. Tests that want a
//! hook to fire only on a specific thread install it on that thread;
//! tests that want global behavior install it on every thread that may
//! participate (typically each worker plus the main thread).
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
    static ON_PARK_HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

// ── Install / clear API ───────────────────────────────────────────

/// Install a hook to fire as the `Blocked` arm enters the per-reason
/// register-waker block, BEFORE `register_*_waker_guard`. The tag
/// distinguishes recv / send / select / join / io. Returns the
/// previously-installed hook (if any).
pub fn install_on_park(hook: Hook) -> Option<Hook> {
    ON_PARK_HOOK.with(|cell| cell.borrow_mut().replace(hook))
}

/// Clear every hook on the calling thread. Test tear-down convenience
/// so a panicking test cannot leak a hook into the next test (which
/// would run on the same OS thread under `cargo test`).
pub fn clear_all() {
    ON_PARK_HOOK.with(|cell| cell.borrow_mut().take());
}

// ── Fire entry points (called by the `fire_hook!` macro) ──────────
//
// The entry point is a free function rather than a closure capture
// because the macro at the call site has no access to the
// `thread_local!`'s `with` plumbing. The body is deliberately tiny so
// the no-hook fast path is a single thread-local read + None check
// (no allocation, no extra indirection).

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn no_hook_installed_is_noop() {
        clear_all();
        on_park("anything");
        // Reaching here without panic means the no-op fast path works.
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
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        install_on_park(Box::new(move |tag| {
            *last_tag2.lock() = Some(tag);
            count2.fetch_add(1, Ordering::SeqCst);
        }));
        on_park("blocked_arm_entry_recv");
        assert_eq!(*last_tag.lock(), Some("blocked_arm_entry_recv"));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        clear_all();
    }

    #[test]
    fn hooks_are_thread_local() {
        clear_all();
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        install_on_park(Box::new(move |_| {
            count2.fetch_add(1, Ordering::SeqCst);
        }));
        // Hook installed on this thread fires here.
        on_park("blocked_arm_entry_recv");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // Hook NOT installed on a sibling thread does not fire there.
        let count3 = count.clone();
        std::thread::spawn(move || {
            on_park("blocked_arm_entry_recv"); // No hook on this thread.
            assert_eq!(count3.load(Ordering::SeqCst), 1);
        })
        .join()
        .unwrap();
        clear_all();
    }
}
