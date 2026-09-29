// This regression test requires the `test-hooks` feature so the
// scheduler exposes the `F10_PARK_SETUP_PAUSE_US` atomic (gated
// on `cfg(any(test, feature = "test-hooks"))` in src/scheduler.rs).
// Without the feature, the atomic is configured out of the `silt`
// crate and this test's `use` line would not resolve.
//
// Run with: `cargo test --features test-hooks --test
// scheduler_cancel_setup_race_tests`. The plain `cargo test`
// command skips this test entirely (the whole test file is
// cfg'd out).
#![cfg(feature = "test-hooks")]

//! Regression test for finding **F10**: `task.cancel(h)` race during
//! the scheduler's per-arm cancel-cleanup setup can panic the worker
//! with `task_slot just initialized`.
//!
//! ## Race
//!
//! In `src/scheduler.rs`'s `Blocked` match arm (Receive / Send /
//! Select), three historical sites cloned the task's handle via:
//!
//! ```ignore
//! let handle_for_cancel = task_slot
//!     .lock()
//!     .as_ref()
//!     .expect("task_slot just initialized")   // ← panics on cancel-mid-setup
//!     .handle
//!     .clone();
//! ```
//!
//! An earlier step (`:626`) installs a generic cancel-cleanup closure
//! that DRAINS `task_slot` when `task.cancel(h)` fires on the handle.
//! If a concurrent `task.cancel(h)` fires between `:626` and the
//! `.expect(...)` above — i.e. after the cleanup is registered but
//! before the per-arm code reaches its handle clone — the cleanup
//! takes the task out of `task_slot`, leaving `None`. The worker
//! thread then panics on `.expect`, producing:
//!
//! ```text
//! thread '<unnamed>' panicked at src/scheduler.rs:NNN:M:
//! task_slot just initialized
//! ```
//!
//! The pre-existing comment at `:689-697` documents the
//! inline-fire-during-register race but does NOT cover this
//! concurrent-cancel variant.
//!
//! ## Fix shape
//!
//! Each arm's `.expect(...)` is replaced with
//! `match task_slot.lock().as_ref() { Some(task) => ..., None => ... }`.
//! On `None` (cancelled-mid-setup) the arm skips waker registration,
//! skips cleanup install, calls `wake_graph.on_wake(NodeId::Task(id))`
//! to remove any phantom park edge inserted at `:679`, and pulses
//! `signal_progress` so main waiters re-check.
//!
//! ## Deterministic reproduction
//!
//! The race window (between `:626` and the per-arm handle clone) is
//! only a handful of nanoseconds in production — effectively
//! unreachable by pure stress. To turn F10 into a reliable 1-of-1
//! failure, the scheduler carries a test-only knob:
//!
//! ```ignore
//! silt::scheduler::F10_PARK_SETUP_PAUSE_US: AtomicU64
//! ```
//!
//! Workers read this on every Receive / Send / Select arm entry AFTER
//! the initial cancel cleanup at `:626` is installed, and `sleep` for
//! that many microseconds BEFORE reaching the per-arm handle clone.
//! Setting it to e.g. 2000µs (2 ms) widens the race window from
//! nanoseconds to milliseconds, so any concurrent `task.cancel(h)`
//! fires reliably inside the window. The atomic is gated on
//! `cfg(any(test, feature = "test-hooks"))` — release builds compile
//! the pause out entirely.
//!
//! With the pause set, even a single spawn-then-cancel pair
//! reproduces the F10 panic pre-fix. This test sets the pause at
//! start-of-test and clears it at end-of-test; between, it runs a
//! modest number of spawn/cancel trials and asserts NO worker panic.

use std::panic;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use silt::scheduler::F10_PARK_SETUP_PAUSE_US;
use silt::scheduler::test_hooks;
use silt::scheduler::test_support::{InProcessRunner, TrialOutcome};

/// Process-wide flag set by the panic hook when any thread panics with
/// a message containing "task_slot just initialized". Worker-thread
/// panics do NOT propagate into the test runner thread's `catch_unwind`
/// (they are distinct OS threads spawned by the scheduler), so the
/// test relies on a global panic hook to observe them.
static F10_PANIC_SEEN: AtomicBool = AtomicBool::new(false);
/// Last panic message seen — for assertion diagnostics. Stored under a
/// Mutex rather than an `AtomicPtr` for convenience; contention is
/// trivial because the hook writes once per panic.
static F10_PANIC_MSG: Mutex<Option<String>> = Mutex::new(None);
/// Most-general panic-message pattern; serves as a secondary signal
/// for ANY worker-thread panic (including an F10 panic whose message
/// formatter was changed but whose file:line still matches the arm).
static F10_ANY_WORKER_PANIC: AtomicBool = AtomicBool::new(false);

/// Install a process-wide panic hook that records any panic carrying
/// the F10 signature. Idempotent — later calls replace earlier hooks
/// so a re-run in the same test process works.
fn install_panic_hook() {
    F10_PANIC_SEEN.store(false, Ordering::SeqCst);
    F10_ANY_WORKER_PANIC.store(false, Ordering::SeqCst);
    *F10_PANIC_MSG.lock().unwrap() = None;
    // Chain to the default hook so stderr still shows the panic
    // (helpful when debugging a regression that happens to fire a
    // different worker-thread panic — we still want to see it).
    let prev = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let msg = format!("{info}");
        // A worker panic is one whose location is in the scheduler's
        // source file. We don't filter to just the scheduler here —
        // any unexpected worker panic is a regression — but we record
        // the F10 fragment specifically so the assertion can disambiguate.
        if msg.contains("task_slot just initialized") {
            F10_PANIC_SEEN.store(true, Ordering::SeqCst);
            *F10_PANIC_MSG.lock().unwrap() = Some(msg.clone());
        }
        // Any worker-thread panic at all is a red flag for F10-style
        // regressions. We mark it but let the specific-message check
        // be the primary signal.
        if msg.contains("scheduler.rs") {
            F10_ANY_WORKER_PANIC.store(true, Ordering::SeqCst);
        }
        prev(info);
    }));
}

/// Microsecond count to sleep at the top of each Blocked arm, after the
/// initial cancel cleanup is installed. 2 ms is generous — long enough
/// to reliably hit the race window even under heavy CI jitter, short
/// enough that the full test runs in under a minute.
const PARK_SETUP_PAUSE_US: u64 = 2_000;

/// Assert that the trial did not produce the F10 panic shape. A
/// timeout or generic deadlock is NOT a failure for this test — we
/// only care about the worker-thread panic that the
/// `.expect("task_slot just initialized")` would fire on cancel-
/// during-setup.
///
/// The check reads the process-wide `F10_PANIC_SEEN` atomic populated
/// by the installed panic hook. Worker-thread panics do not propagate
/// to the test runner thread's `catch_unwind`, so the panic-hook
/// channel is the only reliable way to observe them.
fn assert_no_scheduler_cancel_race_panic(trial: usize, label: &str, outcome: &TrialOutcome) {
    if F10_PANIC_SEEN.load(Ordering::SeqCst) {
        let msg = F10_PANIC_MSG.lock().unwrap().clone().unwrap_or_default();
        panic!(
            "{label} trial {trial}: F10 scheduler cancel-setup race \
             panic detected on a WORKER thread ('task_slot just \
             initialized'); panic message: {msg}; outcome={outcome:?}",
        );
    }
    // Also catch any unexpected worker-thread panic in scheduler.rs.
    assert!(
        !F10_ANY_WORKER_PANIC.load(Ordering::SeqCst),
        "{label} trial {trial}: worker thread panicked in scheduler.rs \
         (not F10's exact message but still a scheduler regression); \
         outcome={outcome:?}",
    );
    // And keep the existing guards on VM-thread panics / error messages
    // — so a future regression that DOES surface on the main VM thread
    // is still caught.
    let msg = outcome.error_message.as_deref().unwrap_or("");
    assert!(
        !msg.contains("task_slot just initialized"),
        "{label} trial {trial}: F10 panic surfaced on the VM thread; \
         outcome={outcome:?}",
    );
    assert!(
        !outcome.saw_panic(),
        "{label} trial {trial}: VM thread panicked; outcome={outcome:?}",
    );
}

/// RAII guard that sets `F10_PARK_SETUP_PAUSE_US` on construction and
/// clears it (back to 0) on drop — so a panicking test body cannot
/// leak the pause into sibling tests (which would slow them down but
/// not produce wrong answers).
struct PauseGuard;

impl PauseGuard {
    fn new(us: u64) -> Self {
        test_hooks::clear_all();
        install_panic_hook();
        F10_PARK_SETUP_PAUSE_US.store(us, Ordering::SeqCst);
        Self
    }
}

impl Drop for PauseGuard {
    fn drop(&mut self) {
        F10_PARK_SETUP_PAUSE_US.store(0, Ordering::SeqCst);
        test_hooks::clear_all();
    }
}

/// **Receive-arm cancel-setup race.** Spawn a receiver on a rendezvous
/// channel (no sender, so the task reliably parks in
/// `BlockReason::Receive`), then immediately cancel it. With the
/// `F10_PARK_SETUP_PAUSE_US` knob set, the worker sleeps inside the
/// Receive arm AFTER installing the initial cancel cleanup (`:626`)
/// but BEFORE reaching the per-arm handle clone — so `task.cancel(h)`
/// fires the cleanup during the sleep. The cleanup drains
/// `task_slot`; when the worker resumes, the arm sees `None`.
///
/// Pre-fix: `.expect("task_slot just initialized")` panics the worker.
/// Post-fix: the `if let Some(task)` gracefully takes the cancelled-
/// mid-setup path, tears down the phantom park edge, and exits.
#[test]
fn test_cancel_during_blocked_receive_setup_does_not_panic_worker() {
    let _guard = PauseGuard::new(PARK_SETUP_PAUSE_US);

    let src = r#"
import channel
import task
import time

fn main() {
  let ch = channel.new(0)
  let h = task.spawn(fn() {
    match channel.receive(ch) {
      Message(_) -> 0
      Closed -> 0
      Empty -> 0
      Sent -> 0
    }
  })
  -- Give the worker time to reach the Blocked-arm setup and enter
  -- the F10_PARK_SETUP_PAUSE_US sleep. 1ms is enough — the pause
  -- is 2ms, and we just need to be INSIDE the pause window when
  -- cancel fires.
  time.sleep(time.ms(1))
  task.cancel(h)
  -- Give the cancellation a moment to unwind (the worker resumes
  -- after the pause and should NOT panic).
  time.sleep(time.ms(10))
  0
}
"#;
    // A single trial is enough when the pause is set — the race is
    // deterministic. Run a modest iteration count for extra
    // confidence and CI-jitter robustness.
    const ITERATIONS: usize = 20;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(3));
    for i in 0..ITERATIONS {
        let outcome = runner.run_trial();
        assert_no_scheduler_cancel_race_panic(i, "recv-arm cancel-setup", &outcome);
    }
}

/// **Send-arm cancel-setup race.** Symmetric to the receive test: the
/// cancelled task blocks in `BlockReason::Send` instead of
/// `BlockReason::Receive`. Pre-fix, the `.expect(...)` on the Send
/// arm panics the worker when the cancel races the arm's handle
/// clone; post-fix the `if let Some` path handles it gracefully.
#[test]
fn test_cancel_during_blocked_send_setup_does_not_panic_worker() {
    let _guard = PauseGuard::new(PARK_SETUP_PAUSE_US);

    let src = r#"
import channel
import task
import time

fn main() {
  let ch = channel.new(0)
  let h = task.spawn(fn() { channel.send(ch, 7) })
  time.sleep(time.ms(1))
  task.cancel(h)
  time.sleep(time.ms(10))
  0
}
"#;
    const ITERATIONS: usize = 20;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(3));
    for i in 0..ITERATIONS {
        let outcome = runner.run_trial();
        assert_no_scheduler_cancel_race_panic(i, "send-arm cancel-setup", &outcome);
    }
}

/// **Select-arm cancel-setup race.** The victim task parks in
/// `BlockReason::Select` over two rendezvous channels. Pre-fix, the
/// `.expect(...)` on the Select arm panics the worker when the
/// cancel races the arm's handle clone; post-fix the `if let Some`
/// path handles it gracefully and notably does NOT leave any waker
/// registered on the select channels (the `entries` vec is still
/// empty at the F10 point, so the branch is a clean exit).
#[test]
fn test_cancel_during_blocked_select_setup_does_not_panic_worker() {
    let _guard = PauseGuard::new(PARK_SETUP_PAUSE_US);

    let src = r#"
import channel
import task
import time

fn main() {
  let a = channel.new(0)
  let b = channel.new(0)
  let h = task.spawn(fn() {
    match channel.select([Recv(a), Recv(b)]) {
      (_, Message(_v)) -> 0
      (_, Closed) -> 0
      _ -> 0
    }
  })
  time.sleep(time.ms(1))
  task.cancel(h)
  time.sleep(time.ms(10))
  0
}
"#;
    const ITERATIONS: usize = 20;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(3));
    for i in 0..ITERATIONS {
        let outcome = runner.run_trial();
        assert_no_scheduler_cancel_race_panic(i, "select-arm cancel-setup", &outcome);
    }
}
