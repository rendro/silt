// Round-75 VM-1 regression tests for the I/O Blocked-arm watchdog
// cleanup mismatch (F10 race window).
//
// Pre-fix: in `src/scheduler.rs`'s `Some(BlockReason::Io(completion))`
// match arm, the initial cancel cleanup at `:669` was set BEFORE the
// per-arm `inner.watchdog.add(...)`. If `task.cancel(h)` fired
// between the cleanup install and the watchdog.add, the cleanup
// would call `watchdog.remove(id)` against an empty registry (no-op),
// then the per-arm code would `add()` an entry that no later
// `remove()` would balance: the F10 cancelled-mid-setup else branch
// exited without removing the entry, claiming (incorrectly, in the
// comment that previously read "the watchdog entry installed above
// is removed by the initial cleanup at :669") that the initial
// cleanup had already cleared it.
//
// The race only manifests when an entry was actually `add()`ed
// during the racy setup. With the cancel cleanup at :669 draining
// `task_slot` before the per-arm code reads `t.vm.current_deadline`,
// `task_deadline` reads as `None` — so the only path that produces
// `effective = Some(...)` (and therefore an `add` that has no
// remove) is when `inner.global_io_timeout` (from `SILT_IO_TIMEOUT`)
// is set. The leak-trigger test below sets `SILT_IO_TIMEOUT` to a
// generous value so `global_deadline` is `Some` regardless of
// whether `task_slot` was drained mid-setup.
//
// Post-fix: the F10 cancelled-mid-setup else branch now calls
// `inner.watchdog.remove(id)` when `effective.is_some()`. `remove`
// is a no-op when the entry is absent, so it is safe in either
// race ordering. The Io arm also gained an `f10_park_setup_pause()`
// call mirroring the Recv/Send/Select arms, so this test can
// deterministically widen the race window via
// `F10_PARK_SETUP_PAUSE_US`.
//
// Verification: tracks adds vs removes via the process-wide
// `WATCHDOG_ADD_COUNT` / `WATCHDOG_REMOVE_COUNT` atomics
// (test-hooks-gated, bumped from inside `WatchdogRegistry::add` and
// `remove`). After the cancel resolves and the scheduler quiesces,
// the test asserts `add_count == remove_count`. A leak shows up as
// `add_count > remove_count`.
//
// Run with: `cargo test --features test-hooks --test
// round75_io_watchdog_f10_race_tests`. The plain `cargo test` skips
// this file (it is `cfg`'d out without the feature).

#![cfg(feature = "test-hooks")]

use std::sync::atomic::Ordering;
use std::time::Duration;

use silt::scheduler::test_hooks;
use silt::scheduler::test_support::InProcessRunner;
use silt::scheduler::{F10_PARK_SETUP_PAUSE_US, WATCHDOG_ADD_COUNT, WATCHDOG_REMOVE_COUNT};

/// Microsecond pause set on `F10_PARK_SETUP_PAUSE_US` to widen the
/// Io-arm race window so a subsequent `task.cancel(h)` reliably fires
/// inside it. 2 ms is the same value the round-31 F10 regression
/// test (`scheduler_cancel_setup_race_tests.rs`) uses.
const PARK_SETUP_PAUSE_US: u64 = 2_000;

/// Serialize env-var manipulation across tests. `cargo test` runs
/// integration tests in parallel by default, and `SILT_IO_TIMEOUT` is
/// process-global. Without serialization, two tests setting/reading
/// it stomp on each other.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard: resets the counters and the pause atomic at start,
/// restores at end, and takes the env lock so SILT_IO_TIMEOUT setting
/// does not race a sibling test.
struct PauseGuard {
    _env_lock: std::sync::MutexGuard<'static, ()>,
}

impl PauseGuard {
    fn new(us: u64) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        test_hooks::clear_all();
        WATCHDOG_ADD_COUNT.store(0, Ordering::SeqCst);
        WATCHDOG_REMOVE_COUNT.store(0, Ordering::SeqCst);
        F10_PARK_SETUP_PAUSE_US.store(us, Ordering::SeqCst);
        // SAFETY: env-var mutation is serialized via ENV_LOCK; the
        // env-cache convention used by Rust 1.80+ makes this safe in
        // the absence of concurrent readers within this process.
        unsafe { std::env::remove_var("SILT_IO_TIMEOUT") };
        Self { _env_lock: lock }
    }

    /// Set SILT_IO_TIMEOUT to a generous value so every Io arm
    /// scheduling produces `effective = Some(global_deadline)` — the
    /// only configuration where the F10 leak manifests deterministically
    /// (since the per-task deadline read goes through `task_slot` and
    /// reads `None` when the cleanup has drained the slot).
    fn set_global_io_timeout(&self, value: &str) {
        // SAFETY: see PauseGuard::new.
        unsafe { std::env::set_var("SILT_IO_TIMEOUT", value) };
    }
}

impl Drop for PauseGuard {
    fn drop(&mut self) {
        F10_PARK_SETUP_PAUSE_US.store(0, Ordering::SeqCst);
        // SAFETY: see PauseGuard::new.
        unsafe { std::env::remove_var("SILT_IO_TIMEOUT") };
        test_hooks::clear_all();
    }
}

/// Drive the Io Blocked arm under the F10-pause widener and assert
/// the per-trial watchdog add/remove counts are balanced. The Silt
/// program below spawns a task that performs a slow I/O and
/// immediately cancels it during the F10 pause window.
///
/// The F10 race trigger requires `effective = Some(...)` at the
/// per-arm `inner.watchdog.add` call site; with `task_slot` drained
/// mid-setup, only `global_io_timeout` (set via SILT_IO_TIMEOUT)
/// produces a non-None `effective`. The 60s timeout never fires;
/// the cancel resolves the task long before the I/O completes.
///
/// Pre-fix behaviour (without round-75 VM-1): the Io arm calls
/// `inner.watchdog.add(...)` on every Io park where
/// `global_io_timeout` is set; on the F10 cancel-mid-setup path,
/// the cleanup at :669 already drained the slot and ran
/// `watchdog.remove(id)` (no-op pre-add). The else branch then
/// exited without remove, leaking one entry per trial.
///
/// Post-fix: the else branch calls `inner.watchdog.remove(id)`
/// when `effective.is_some()`, so adds == removes.
#[test]
fn test_io_arm_cancel_setup_race_does_not_leak_watchdog_entry() {
    let guard = PauseGuard::new(PARK_SETUP_PAUSE_US);
    // 60s — far in the future, never fires; we only need
    // `global_deadline` to be `Some` so `effective.is_some()` at
    // the watchdog.add site even when task_slot was drained.
    guard.set_global_io_timeout("60s");

    // Reset counters AFTER setting env var so a transient scheduler
    // rebuild does not skew the baseline.
    WATCHDOG_ADD_COUNT.store(0, Ordering::SeqCst);
    WATCHDOG_REMOVE_COUNT.store(0, Ordering::SeqCst);

    // The program parks a child task on a slow I/O (read of a file
    // that does not exist; the I/O pool will return Err quickly,
    // but the worker still parks on `BlockReason::Io(_)` and goes
    // through the Io arm, which is what we need to exercise).
    //
    // The 1ms time.sleep before task.cancel(h) gives the spawned
    // task time to reach the Io arm and enter the 2ms F10 pause;
    // cancel then fires deterministically inside the pause.
    let src = r#"
import io
import task
import time

fn main() {
  let h = task.spawn(fn() {
    let _ = io.read_file("/tmp/silt_round75_vm1_does_not_exist.txt")
    0
  })
  time.sleep(time.ms(1))
  task.cancel(h)
  time.sleep(time.ms(50))
  0
}
"#;

    // Several trials so a flaky pre-fix has multiple chances to
    // leak. Each trial creates a fresh Vm/Scheduler so the global
    // counters accumulate per-trial.
    const ITERATIONS: usize = 5;
    let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(5));
    for i in 0..ITERATIONS {
        let outcome = runner.run_trial();
        assert!(
            !outcome.saw_panic(),
            "trial {i}: VM thread panicked; outcome={outcome:?}"
        );
    }

    // After all trials and a small settle pause, total adds must
    // equal total removes. A leak (pre-fix) shows up as
    // `removes < adds` — typically by exactly N (one entry per
    // trial that hit the F10 else branch).
    std::thread::sleep(Duration::from_millis(100));
    let adds = WATCHDOG_ADD_COUNT.load(Ordering::SeqCst);
    let removes = WATCHDOG_REMOVE_COUNT.load(Ordering::SeqCst);
    assert!(
        adds > 0,
        "expected at least one watchdog.add to fire; got adds={adds}, \
         removes={removes}. The Io arm may not have been reached, or \
         SILT_IO_TIMEOUT may not have been read by Scheduler::new."
    );
    assert_eq!(
        adds,
        removes,
        "round-75 VM-1: I/O Blocked-arm cancel-setup race leaked \
         {} watchdog registry entries (adds={adds}, removes={removes}). \
         The F10 cancelled-mid-setup else branch must call \
         `inner.watchdog.remove(id)` when `effective.is_some()`.",
        adds.saturating_sub(removes)
    );
}

/// Sanity sibling: with the F10 pause cleared, the same program
/// should still complete cleanly and leave `adds == removes`.
/// Catches a regression that broke the non-race path (e.g. a
/// double-remove that fires on every Io arm exit).
#[test]
fn test_io_arm_normal_path_balances_watchdog_counters() {
    let guard = PauseGuard::new(0); // no pause; production timing
    guard.set_global_io_timeout("60s");
    WATCHDOG_ADD_COUNT.store(0, Ordering::SeqCst);
    WATCHDOG_REMOVE_COUNT.store(0, Ordering::SeqCst);

    // The path exists, so io.read_file completes normally. The
    // global SILT_IO_TIMEOUT installs `effective = Some(...)`, so
    // watchdog.add fires; on normal completion, the requeue path
    // (`scheduler.rs:1311`) calls watchdog.remove. Adds == removes.
    let path = std::env::temp_dir().join(format!(
        "silt_round75_vm1_normal_{}.txt",
        std::process::id()
    ));
    std::fs::write(&path, "ready").unwrap();
    let path_str = path.display().to_string().replace('\\', "/");
    let src = format!(
        r#"
import io
import task

fn main() {{
  let h = task.spawn(fn() {{
    let _ = io.read_file("{path_str}")
    0
  }})
  task.join(h)
  0
}}
"#
    );

    const ITERATIONS: usize = 3;
    let runner = InProcessRunner::new(&src).with_budget(Duration::from_secs(5));
    for i in 0..ITERATIONS {
        let outcome = runner.run_trial();
        assert!(
            !outcome.saw_panic(),
            "trial {i}: VM thread panicked; outcome={outcome:?}"
        );
    }
    let _ = std::fs::remove_file(&path);

    std::thread::sleep(Duration::from_millis(50));
    let adds = WATCHDOG_ADD_COUNT.load(Ordering::SeqCst);
    let removes = WATCHDOG_REMOVE_COUNT.load(Ordering::SeqCst);
    assert_eq!(
        adds, removes,
        "normal-path Io arm should balance watchdog adds/removes; \
         got adds={adds}, removes={removes}",
    );
}
