//! Regression tests for the scheduler's deadlock detector false-positive
//! on a legitimate fan-in pattern.
//!
//! # The bug (pre-fix)
//!
//! The main thread's channel watchdog — see
//! `main_thread_wait_for_receive` / `main_thread_wait_for_send` in
//! `src/builtins/concurrency.rs` — ticks every 100ms and declares a
//! deadlock when every live scheduled task is currently parked on an
//! internal graph edge (channel / select / join). The "live vs. blocked"
//! sample was taken with three separate atomic loads, so the detector
//! was blind to tasks that had been submitted (or requeued) but not
//! yet picked up by a worker thread. On rendezvous fan-in shapes —
//! main spawns N senders, then enters a recv loop — that race produced
//! intermittent false positives on ~1–5% of Linux runs and substantially
//! more on Windows:
//!
//! ```text
//! error[runtime]: deadlock on main thread: channel receive with no counterparty
//! ```
//!
//! # The fix
//!
//! `SchedulerInner` gained a third atomic counter, `unsettled_tasks`,
//! that tracks the lifetime of a task from "enqueued" to "settled". A
//! task is unsettled from the moment it enters the run queue (submit /
//! requeue) until the worker that runs it has either completed it or
//! parked it on a wakeable edge with a registered waker. Crucially, the
//! counter is NOT decremented at `pop_front` — it stays positive across
//! the dequeue → register-waker window, which is exactly where the
//! false-positive used to fire.
//!
//! `Scheduler::can_make_progress` short-circuits to `true` while
//! `unsettled_tasks > 0`: the scheduler has unobserved work in flight
//! and is definitionally not deadlocked.
//!
//! # What these tests lock
//!
//! * `test_fan_in_16_not_false_deadlock` — the minimal repro from the
//!   bug report. Every trial must exit 0 and print `sum=136`; the fan-in
//!   race is no longer tolerated as a flake.
//! * `test_real_deadlock_still_detected` — a main-thread receive with
//!   no sender anywhere (no scheduler, no spawned task). The detector
//!   must still fire so legitimate bugs are surfaced.
//! * `test_real_deadlock_detected_after_spawn_completes_without_sending`
//!   — a spawned task that returns without sending. Once the task's
//!   `live_tasks` decrement settles, `unsettled_tasks == 0` and
//!   `live == blocked == 0`, so the detector must fire.
//! * `test_unsettled_tasks_held_across_dequeue_to_waker_registration` —
//!   regression lock that fails if anyone moves the
//!   `unsettled_tasks` decrement back to `pop_front`. Spins many fan-in
//!   trials and asserts zero false-positive panics across N trials.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_silt_file(stem: &str, src: &str) -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tmp = std::env::temp_dir().join(format!(
        "silt_sched_deadlock_{}_{}_{}_{}.silt",
        stem,
        std::process::id(),
        ts,
        n
    ));
    std::fs::write(&tmp, src).unwrap();
    tmp
}

struct RunResult {
    stdout: String,
    stderr: String,
    exit: Option<i32>,
    timed_out: bool,
}

fn run_silt(stem: &str, src: &str, max_wall: Duration) -> RunResult {
    let tmp = tmp_silt_file(stem, src);
    let start = Instant::now();
    let mut child = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn silt binary");

    let mut exit_code: Option<i32> = None;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => {
                if start.elapsed() > max_wall {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break;
            }
        }
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut s) = child.stdout.take() {
        use std::io::Read;
        let _ = s.read_to_string(&mut stdout);
    }
    if let Some(mut s) = child.stderr.take() {
        use std::io::Read;
        let _ = s.read_to_string(&mut stderr);
    }

    let _ = std::fs::remove_file(&tmp);

    RunResult {
        stdout,
        stderr,
        exit: exit_code,
        timed_out,
    }
}

/// **Fan-in: 16 senders → main receive loop.** The minimal reproducer
/// from the bug report. Pre-fix, ~1-5% of Linux trials (and more on
/// Windows) produced
/// `error[runtime]: deadlock on main thread: channel receive with no counterparty`
/// because main's watchdog sampled the counters between `submit` and
/// worker pickup. Post-fix, `unsettled_tasks > 0` keeps the detector
/// quiet across the dequeue → register-waker window, so every trial
/// reaches `sum=136`.
///
/// Iteration count: 20 trials on every platform. STRICT per-trial
/// assertion: every trial must exit 0 with `sum=136`. The earlier
/// "at least one of 20 iterations" relaxation was a workaround for the
/// residual race that round-31 closed by moving the decrement out of
/// `pop_front` — see the round-31 commit and the new
/// `test_unsettled_tasks_held_across_dequeue_to_waker_registration`.
#[test]
fn test_fan_in_16_not_false_deadlock() {
    let src = r#"
import channel
import list
import task

fn main() {
  let ch = channel.new(0)
  let _senders = 1..16
    |> list.map { i -> task.spawn(fn() { channel.send(ch, i) }) }
  let sum = loop c = 0, acc = 0 {
    match c >= 16 {
      true -> acc
      _ -> match channel.receive(ch) {
        Message(v) -> loop(c + 1, acc + v)
        _ -> acc
      }
    }
  }
  println("sum={sum}")
}
"#;
    // STRICT per-trial assertion: every trial must exit 0 with sum=136
    // and no `deadlock` string in stderr. After the worker-side detector
    // was removed, the only remaining detector is the main-thread
    // watchdog, and main is busy receiving (not idle), so a false
    // positive cannot fire from this shape.
    const ITERATIONS: usize = 20;
    for trial in 0..ITERATIONS {
        let res = run_silt(
            &format!("fan_in_16_not_false_deadlock_{trial}"),
            src,
            Duration::from_secs(15),
        );
        assert!(
            !res.timed_out,
            "trial {trial}: TIMEOUT; stdout={:?} stderr={:?}",
            res.stdout, res.stderr
        );
        assert!(
            !res.stderr.contains("panicked"),
            "trial {trial}: unexpected panic; stderr={}",
            res.stderr,
        );
        assert!(
            !res.stderr.contains("deadlock"),
            "trial {trial}: false-positive deadlock diagnostic; \
             stdout={:?} stderr={:?}",
            res.stdout,
            res.stderr,
        );
        assert!(
            res.stdout.contains("sum=136"),
            "trial {trial}: did not reach sum=136; stdout={:?} stderr={:?}",
            res.stdout,
            res.stderr,
        );
        assert_eq!(
            res.exit,
            Some(0),
            "trial {trial}: non-zero exit {:?}; stdout={:?} stderr={:?}",
            res.exit,
            res.stdout,
            res.stderr,
        );
    }
}

/// **Real deadlock — no sender at all.** Main receives on a channel
/// that nothing can ever send to (no spawned task, no scheduler).
/// The detector must still fire — `unsettled_tasks == 0` always, and
/// `current_scheduler() == None` makes `scheduler_can_make_progress`
/// return `false`. This test guards the fix from trivially disabling
/// deadlock detection.
#[test]
fn test_real_deadlock_still_detected() {
    let src = r#"
import channel

fn main() {
  let ch = channel.new(0)
  match channel.receive(ch) {
    Message(_) -> println("unreachable: received")
    _ -> println("unreachable: closed")
  }
}
"#;
    let res = run_silt("real_deadlock_no_sender", src, Duration::from_secs(10));
    assert!(
        !res.timed_out,
        "expected deadlock error, not timeout; stderr={}",
        res.stderr
    );
    assert_ne!(
        res.exit,
        Some(0),
        "expected non-zero exit (deadlock error); stdout={:?} stderr={:?}",
        res.stdout,
        res.stderr,
    );
    assert!(
        res.stderr.contains("deadlock") && res.stderr.contains("no counterparty"),
        "expected the 'deadlock on main thread' diagnostic; stderr={}",
        res.stderr,
    );
    assert!(
        !res.stdout.contains("unreachable"),
        "main must not have proceeded past receive; stdout={}",
        res.stdout,
    );
}

/// **Real deadlock — spawn completes without sending.** A scheduled
/// task exists briefly (so the scheduler is created and
/// `unsettled_tasks` transiently bumps), then returns without ever
/// sending. Main blocks on the channel forever. Once the spawned task
/// completes, `live_tasks` drops to 0 and `unsettled_tasks` drops to 0,
/// so the detector must fire. This test guards the case where
/// `unsettled_tasks > 0` keeps the detector quiet during the spawn
/// window but does NOT permanently suppress deadlock detection.
#[test]
fn test_real_deadlock_detected_after_spawn_completes_without_sending() {
    let src = r#"
import channel
import task

fn main() {
  let ch = channel.new(0)
  -- Spawn a task that does NOT send — it just returns immediately.
  -- This exercises the case where unsettled_tasks transiently > 0 but
  -- the scheduler settles into a genuinely-deadlocked state once the
  -- task completes.
  let _h = task.spawn(fn() { 1 })
  match channel.receive(ch) {
    Message(_) -> println("unreachable: received")
    _ -> println("unreachable: closed")
  }
}
"#;
    let res = run_silt("real_deadlock_spawn_no_send", src, Duration::from_secs(10));
    assert!(
        !res.timed_out,
        "expected deadlock error, not timeout; stderr={}",
        res.stderr
    );
    assert_ne!(
        res.exit,
        Some(0),
        "expected non-zero exit (deadlock error); stdout={:?} stderr={:?}",
        res.stdout,
        res.stderr,
    );
    assert!(
        res.stderr.contains("deadlock") && res.stderr.contains("no counterparty"),
        "expected the 'deadlock on main thread' diagnostic; stderr={}",
        res.stderr,
    );
    assert!(
        !res.stdout.contains("unreachable"),
        "main must not have proceeded past receive; stdout={}",
        res.stdout,
    );
}

/// **Detector still fires within reasonable time on a constructed
/// unsolvable deadlock.** Stricter timing lock for the case above:
/// regardless of the `unsettled_tasks` re-shaping, the detector must
/// still surface a real deadlock within a few seconds. If the fix
/// regressed the detector to be permanently silent (e.g. by holding
/// `unsettled_tasks` non-zero forever in some path), this test would
/// hit the wall-clock timeout instead of the deadlock diagnostic.
///
/// Two spawned tasks both block on receives that nobody ever sends to.
/// Once both have parked with their wakers, `unsettled_tasks` is back
/// to zero and `internal_blocked == live`, so the next watchdog tick
/// MUST fire the deadlock diagnostic. We give the run 8s — far more
/// than enough for two `submit → settle` cycles plus the 1s watchdog
/// tick.
#[test]
fn test_detector_fires_within_reasonable_time_on_real_deadlock() {
    let src = r#"
import channel
import task

fn main() {
  let ch = channel.new(0)
  let _h1 = task.spawn(fn() {
    match channel.receive(ch) {
      Message(_) -> 0
      _ -> 0
    }
  })
  let _h2 = task.spawn(fn() {
    match channel.receive(ch) {
      Message(_) -> 0
      _ -> 0
    }
  })
  -- Main also blocks on the same channel; nobody ever sends.
  match channel.receive(ch) {
    Message(_) -> println("unreachable: received")
    _ -> println("unreachable: closed")
  }
}
"#;
    let started = Instant::now();
    let res = run_silt(
        "detector_fires_within_reasonable_time",
        src,
        Duration::from_secs(8),
    );
    let elapsed = started.elapsed();
    assert!(
        !res.timed_out,
        "detector did not fire within 8s — fix may have made it permanently \
         silent; elapsed={:?} stderr={}",
        elapsed, res.stderr,
    );
    assert_ne!(res.exit, Some(0), "expected non-zero exit (deadlock)");
    assert!(
        res.stderr.contains("deadlock"),
        "expected deadlock diagnostic; stderr={}",
        res.stderr,
    );
    assert!(
        !res.stdout.contains("unreachable"),
        "main must not have proceeded past receive; stdout={}",
        res.stdout,
    );
}

/// **Regression lock for the round-31 settle-window fix.** This test
/// MUST FAIL if a future change moves the `unsettled_tasks` decrement
/// back into `pop_front` (or any pre-settle site). The shape is the
/// minimal fan-in repro from `test_fan_in_16_not_false_deadlock`,
/// run a high N times so the round-30 race shape (decrement at
/// `pop_front`) would have surfaced at least one false-positive
/// deadlock with high probability.
///
/// Pre-fix (round-30 shape) failure rate: ~1-5% per Linux trial,
/// substantially higher on Windows. With N = 50, P(at least one
/// failure) ≈ 1 - 0.95^50 ≈ 92% on the low end, ≈ 99.99%+ on the
/// high end. With N = 100 the lower-bound probability is ≈ 99.4%.
/// Post-fix the counter stays positive across the dequeue →
/// register-waker window so EVERY trial must reach `sum=136` with
/// no deadlock diagnostic.
///
/// Reasoning-from-public-API style: we don't poke `unsettled_tasks`
/// directly — we observe the detector's behavior on a shape that
/// forces the race. If the decrement leaks back into `pop_front`,
/// this assertion will start failing; if the regression is tiny
/// (single trial out of 100) we will still see it because EVERY
/// trial must succeed.
// Now passes on every platform (including Windows): the worker-side
// detector that produced the residual race was removed, so the
// `unsettled_tasks` invariant is no longer the only thing standing
// between the workers and a false-positive deadlock fire — there is no
// worker-side fire path at all.
#[test]
fn test_unsettled_tasks_held_across_dequeue_to_waker_registration() {
    let src = r#"
import channel
import list
import task

fn main() {
  let ch = channel.new(0)
  let _senders = 1..16
    |> list.map { i -> task.spawn(fn() { channel.send(ch, i) }) }
  let sum = loop c = 0, acc = 0 {
    match c >= 16 {
      true -> acc
      _ -> match channel.receive(ch) {
        Message(v) -> loop(c + 1, acc + v)
        _ -> acc
      }
    }
  }
  println("sum={sum}")
}
"#;
    // 50 trials. Pre-round-31 shape: P(at least one false-positive)
    // ≥ 92% on Linux, near-certain on Windows. Post-fix: 0/50 must
    // see a deadlock or any panic.
    const ITERATIONS: usize = 50;
    let mut deadlock_diagnostics: Vec<(usize, String, String)> = Vec::new();
    let mut wrong_sums: Vec<(usize, String)> = Vec::new();
    for trial in 0..ITERATIONS {
        let res = run_silt(
            &format!("unsettled_held_across_dequeue_{trial}"),
            src,
            Duration::from_secs(15),
        );
        assert!(
            !res.timed_out,
            "trial {trial}: TIMEOUT (the regression test should never hang); \
             stdout={:?} stderr={:?}",
            res.stdout, res.stderr
        );
        assert!(
            !res.stderr.contains("panicked"),
            "trial {trial}: unexpected panic; stderr={}",
            res.stderr,
        );
        if res.stderr.contains("deadlock") || res.stderr.contains("no counterparty") {
            deadlock_diagnostics.push((trial, res.stdout.clone(), res.stderr.clone()));
        }
        if !res.stdout.contains("sum=136") {
            wrong_sums.push((trial, res.stdout.clone()));
        }
    }
    assert!(
        deadlock_diagnostics.is_empty(),
        "round-31 regression: {} of {} trials produced a false-positive \
         deadlock diagnostic. The `unsettled_tasks` decrement appears to \
         have leaked back into `pop_front` (or some other pre-settle \
         site). First failure: trial {:?}, stdout={:?}, stderr={:?}",
        deadlock_diagnostics.len(),
        ITERATIONS,
        deadlock_diagnostics.first().map(|(t, _, _)| *t),
        deadlock_diagnostics.first().map(|(_, s, _)| s.as_str()),
        deadlock_diagnostics.first().map(|(_, _, e)| e.as_str()),
    );
    assert!(
        wrong_sums.is_empty(),
        "round-31 regression: {} of {} trials did not reach sum=136 \
         (without a panic). Failing trials: {:?}",
        wrong_sums.len(),
        ITERATIONS,
        wrong_sums,
    );
}

/// **Regression lock for the round-32 worker-side-detector removal.**
/// Pre-fix shape: a worker thread's `condvar.wait_for(.., 1s)` could
/// time out, observe `unsettled==0`, `live==1`, `internal_blocked==1`,
/// `queue.empty()`, and falsely declare deadlock — even when main was
/// not actually stuck, just busy crunching VM bytecode between recv
/// iterations. On a Windows CI runner under high parallel cargo-test
/// load, main could be descheduled for >1s, making this false positive
/// near-certain on the minimal fan-in shape.
///
/// This test stresses exactly that scenario: 16 sender tasks all park
/// with wakers (so the four worker-visible counters look like deadlock),
/// then main does ~thousands of VM ops between each `channel.receive`
/// call. Pre-fix, every trial that won the descheduling lottery would
/// see `error[runtime]` from the worker thread declaring deadlock.
/// Post-fix (worker-side detector removed), only the main-thread
/// watchdog can fire, and main is busy receiving (not parked on a
/// primitive long enough for the consecutive-tick threshold to elapse),
/// so 0/20 trials may produce a deadlock diagnostic.
#[test]
fn test_no_false_deadlock_when_main_is_busy() {
    let src = r#"
import channel
import list
import task

fn main() {
  let ch = channel.new(0)
  let _senders = 1..16
    |> list.map { i -> task.spawn(fn() { channel.send(ch, i) }) }
  let sum = loop c = 0, acc = 0 {
    -- Tight CPU loop on main between receives — simulates main being
    -- descheduled by the OS. Pre-fix, this gave a worker enough
    -- wall-clock time to time out on its 1s wait_for and falsely
    -- declare deadlock against the four counters that briefly looked
    -- "stuck" while main was crunching this loop.
    let _busy = loop k = 0 {
      match k >= 2000 {
        true -> 0
        _ -> loop(k + 1)
      }
    }
    match c >= 16 {
      true -> acc
      _ -> match channel.receive(ch) {
        Message(v) -> loop(c + 1, acc + v)
        _ -> acc
      }
    }
  }
  println("sum={sum}")
}
"#;
    const ITERATIONS: usize = 20;
    let mut deadlock_diagnostics: Vec<(usize, String, String)> = Vec::new();
    let mut wrong_sums: Vec<(usize, String, String)> = Vec::new();
    for trial in 0..ITERATIONS {
        let res = run_silt(
            &format!("no_false_deadlock_when_main_is_busy_{trial}"),
            src,
            Duration::from_secs(20),
        );
        assert!(
            !res.timed_out,
            "trial {trial}: TIMEOUT; stdout={:?} stderr={:?}",
            res.stdout, res.stderr
        );
        assert!(
            !res.stderr.contains("panicked"),
            "trial {trial}: unexpected panic; stderr={}",
            res.stderr,
        );
        if res.stderr.contains("deadlock") {
            deadlock_diagnostics.push((trial, res.stdout.clone(), res.stderr.clone()));
        }
        if !res.stdout.contains("sum=136") {
            wrong_sums.push((trial, res.stdout.clone(), res.stderr.clone()));
        }
    }
    assert!(
        deadlock_diagnostics.is_empty(),
        "round-32 regression: {}/{} trials produced a false-positive \
         deadlock diagnostic. The worker-side deadlock detector must \
         not have been re-introduced. First failure: {:?}",
        deadlock_diagnostics.len(),
        ITERATIONS,
        deadlock_diagnostics.first(),
    );
    assert!(
        wrong_sums.is_empty(),
        "round-32 regression: {}/{} trials did not reach sum=136. \
         First failure: {:?}",
        wrong_sums.len(),
        ITERATIONS,
        wrong_sums.first(),
    );
}
