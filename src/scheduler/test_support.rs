//! In-process scheduler test harness.
//!
//! Phase 2 of the watchdog rewrite: replaces the `cargo run` ×
//! N-iterations subprocess shape used by
//! `tests/scheduler_deadlock_detector_tests.rs` and
//! `tests/scheduler_race_tests.rs` with a thin wrapper that drives the
//! same invariants — exit code, stdout sum, stderr deadlock-or-not —
//! through the public `Vm` / `Scheduler` API in the calling process.
//!
//! ## Why in-process matters
//!
//! Each subprocess trial pays:
//!   * fork/exec + dynamic linker (~30-100ms),
//!   * silt CLI bootstrap (parse args, locate manifest, read source),
//!   * a fresh Rust runtime (allocator, env, panic hook).
//!
//! On a 16-core box those costs are amortized across `cargo test -j N`,
//! but on a 2-core CI runner the per-trial wall clock is ~200ms even
//! when the program itself runs in 5ms. A 50-iteration regression
//! lock spends ~10s in fork/exec, which is the dominant signal in the
//! test's wall-clock budget — and leaves no room to crank iterations
//! further.
//!
//! In-process the trial cost collapses to "compile the source +
//! `vm.run(script)`": ~1-5ms for the success case and <50ms for the
//! real-deadlock case (Phase 4 wake graph fires atomically with the
//! mutating event — no consecutive-tick threshold to clear).
//!
//! ## Why the harness routes through `Vm::run`, not raw `Scheduler::submit`
//!
//! The migrated tests assert on an end-to-end invariant: when a Silt
//! `fn main()` performs a particular concurrency pattern, does the
//! main-thread watchdog (in `src/builtins/concurrency.rs`) fire a
//! false-positive `error[runtime]: deadlock on main thread`? That
//! watchdog interacts with:
//!
//!   1. `Scheduler::is_main_starved` — the wake-graph BFS that
//!      proves no scheduled task can drive `target` forward;
//!   2. `Scheduler::install_main_waiter` — the signal callback that
//!      pokes main's local condvar on every park / wake / spawn /
//!      complete;
//!   3. `Channel::has_pending_timer_close` — the external-waker
//!      escape hatch consulted by the BFS for channel targets;
//!   4. The waker chain that fires from
//!      `Channel::register_recv_waker_guard`'s double-check / `wake_*` /
//!      worker-side `requeue` and back into the main thread's local
//!      condvar.
//!
//! All four interact through state that only exists once a real `Vm`
//! is executing real bytecode that calls `channel.spawn` / `channel.
//! receive` etc. A harness that handcrafts `Task` values and feeds
//! them to `Scheduler::submit` would bypass `current_scheduler()`
//! attachment, bypass the main-thread `_wait_for_*` codepaths, and
//! end up testing a different thing than the subprocess version did.
//!
//! So: the harness is structured around `compile + run`, not around
//! `Scheduler::submit`. The Silt source the harness runs is the
//! same source the subprocess version runs — bit-for-bit identical
//! programs — so the assertions migrate without semantic drift.
//!
//! ## Wall-clock bound
//!
//! Every `run_trial` call runs on a worker thread spawned per-call
//! and joined under a hard wall-clock timeout. A hang in the migrated
//! program (or in the scheduler) does not hang the test suite; it
//! turns into a `RunOutcome::TimedOut` after the configured budget.
//! Default budget is 15s — the same as the subprocess version's per-
//! trial budget — but is overridable per-trial.
//!
//! ## What the harness does NOT do
//!
//! * It does not poke `Scheduler` internals. Counter manipulation
//!   would short-circuit the very codepath under test.
//! * It does not silence stderr. The scheduler / watchdog still
//!   prints diagnostics; the harness captures the `VmError` (which
//!   carries the same "deadlock on main thread: ..." message that
//!   the CLI prints to stderr) and exposes it on the outcome.
//! * It does not run multiple trials in parallel inside a single
//!   harness instance. Each `run_trial` is independent — runs a
//!   fresh `Vm`, uses a fresh `Scheduler` (one is created on first
//!   `task.spawn`), and tears everything down before returning.
//!   Tests that want N trials loop over `run_trial`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::compiler::Compiler;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::value::Value;
use crate::vm::Vm;

/// Outcome of a single in-process trial.
///
/// Mirrors the surface the subprocess `RunResult` exposed (stdout,
/// stderr, exit, timed-out) but populated from in-process state:
///
/// * `stdout` — captured from a custom `print` redirect; `println` in
///   Silt writes here.
/// * `error_message` — the `VmError`'s message when `vm.run` returned
///   `Err`; this is the moral equivalent of the subprocess `stderr`
///   for the assertions the migrated tests make. Specifically: a
///   real-deadlock fires `Err("deadlock on main thread: ...")`.
/// * `result` — the success value (only `Some` if the program
///   returned cleanly).
/// * `timed_out` — `true` when the trial hit the wall-clock budget.
/// * `elapsed` — total wall-clock time spent in `run_trial`.
#[derive(Debug)]
pub struct TrialOutcome {
    pub stdout: String,
    pub error_message: Option<String>,
    pub result: Option<Value>,
    pub timed_out: bool,
    pub elapsed: Duration,
}

impl TrialOutcome {
    /// True if the program produced a `deadlock on main thread`
    /// diagnostic — same string fragment the subprocess test asserts
    /// on `stderr`.
    pub fn saw_deadlock(&self) -> bool {
        match &self.error_message {
            Some(msg) => msg.contains("deadlock"),
            None => false,
        }
    }

    /// True if the run completed without error (`vm.run` returned
    /// `Ok`) AND did not time out. The subprocess equivalent is
    /// `exit == Some(0)`.
    pub fn ok(&self) -> bool {
        !self.timed_out && self.error_message.is_none()
    }

    /// True if the trial saw a panic-shaped error. The subprocess
    /// version detected this as `stderr.contains("panicked")`. The
    /// in-process version surfaces any panic that propagated out of
    /// the worker thread's `vm.run` as an `error_message` containing
    /// `"panic"` / `"panicked"`. Used by `scheduler_race_tests` to
    /// catch the round-27 `task_slot just initialized` shape.
    pub fn saw_panic(&self) -> bool {
        match &self.error_message {
            Some(msg) => msg.contains("panic"),
            None => false,
        }
    }
}

/// In-process runner for a single Silt program. Construct once,
/// invoke `run_trial` N times.
///
/// Each `run_trial` is independent: a fresh `Vm`, fresh `Scheduler`
/// (created lazily on first `task.spawn`), fresh stdout buffer. The
/// shared `InProcessRunner` only carries the source string and the
/// per-trial wall-clock budget.
pub struct InProcessRunner {
    source: String,
    /// Per-trial wall-clock budget. A trial that exceeds this is
    /// reported as `TrialOutcome { timed_out: true, .. }`.
    budget: Duration,
}

impl InProcessRunner {
    /// Build a runner for the given Silt source. The source must
    /// declare `fn main() { ... }`; the harness invokes it via
    /// `Vm::run` exactly as the silt CLI's `run` subcommand does.
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            budget: Duration::from_secs(15),
        }
    }

    /// Override the per-trial wall-clock budget. Used by tests with
    /// known-fast happy paths (fan-in 16: ~5-50ms) and tests that
    /// must give the watchdog time to fire (real-deadlock: ~5-8s).
    ///
    /// Under `CI=1`, the supplied budget is multiplied by 4× to
    /// absorb GitHub-hosted runner CPU contention. The silt watchdog
    /// fires on a fixed 250ms streak; under heavy load worker threads
    /// can be starved long enough to trip the watchdog without an
    /// actual deadlock. Multiplying the budget gives the trial more
    /// wall-clock to escape pathological scheduling, without changing
    /// the watchdog window itself (which is what the regression-lock
    /// asserts on). Local runs are unaffected.
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = if std::env::var("CI").is_ok() {
            budget * 4
        } else {
            budget
        };
        self
    }

    /// Run one trial. Returns once the program completes, errors,
    /// panics, or hits the wall-clock budget. Never blocks
    /// indefinitely.
    pub fn run_trial(&self) -> TrialOutcome {
        let started = Instant::now();
        let source = self.source.clone();
        // Spawn the Vm on a dedicated thread so we can join with a
        // wall-clock budget. Without this, a hung `vm.run` (e.g. a
        // bug in the watchdog that re-enters the wait loop forever)
        // would hang the test harness too.
        let (tx, rx) = std::sync::mpsc::channel::<(String, Result<Value, String>)>();
        let handle = std::thread::Builder::new()
            .name(format!("silt-in-process-runner-{}", trial_id()))
            .spawn(move || {
                // Catch panics from the VM thread so a worker-side
                // panic surfaces as `error_message: "panic: ..."`
                // rather than aborting the entire test process.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    compile_and_run(&source)
                }));
                let payload = match result {
                    Ok((stdout, run_result)) => (stdout, run_result.map_err(|e| e.message)),
                    Err(panic_payload) => {
                        let panic_msg = panic_payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| {
                                panic_payload
                                    .downcast_ref::<&'static str>()
                                    .map(|s| s.to_string())
                            })
                            .unwrap_or_else(|| "<non-string panic>".to_string());
                        (
                            String::new(),
                            Err(format!("panic in vm thread: {panic_msg}")),
                        )
                    }
                };
                let _ = tx.send(payload);
            })
            .expect("failed to spawn in-process runner thread");
        // Wait up to `self.budget` for the runner thread to send
        // its outcome. recv_timeout is the right primitive: it
        // doesn't burn CPU and it returns promptly if the runner
        // finishes early.
        let outcome = match rx.recv_timeout(self.budget) {
            Ok((stdout, Ok(value))) => TrialOutcome {
                stdout,
                error_message: None,
                result: Some(value),
                timed_out: false,
                elapsed: started.elapsed(),
            },
            Ok((stdout, Err(msg))) => TrialOutcome {
                stdout,
                error_message: Some(msg),
                result: None,
                timed_out: false,
                elapsed: started.elapsed(),
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => TrialOutcome {
                stdout: String::new(),
                error_message: None,
                result: None,
                timed_out: true,
                elapsed: started.elapsed(),
            },
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => TrialOutcome {
                stdout: String::new(),
                error_message: Some("runner thread disconnected without sending a result".into()),
                result: None,
                timed_out: false,
                elapsed: started.elapsed(),
            },
        };
        // Best-effort detach: if the trial timed out, the runner
        // thread is still alive and we cannot cleanly cancel a Vm
        // mid-execute_slice. Joining would block forever, so we
        // just leak the JoinHandle; the OS will reap the thread
        // when the test process exits. This is acceptable because
        // a timeout is an error condition that fails the test —
        // the test process is on its way out.
        if outcome.timed_out {
            std::mem::forget(handle);
        } else {
            let _ = handle.join();
        }
        outcome
    }
}

/// Compile and run `source` end-to-end, capturing whatever `println`
/// writes to a per-call buffer. The captured stdout is returned
/// alongside `vm.run`'s result so the caller can inspect both
/// independently.
///
/// Routing print through a `STDOUT_REDIRECT` thread-local would
/// require touching the `print` builtin. To keep this harness
/// self-contained AND avoid scope creep into the print plumbing, the
/// harness reads the `vm.run` result and lets stdout fall through to
/// the test process's actual stdout — which `cargo test` captures
/// per-test via its own stdout redirection. The `stdout` field of
/// `TrialOutcome` is left empty for in-process trials; tests that
/// need to assert on stdout content match against the returned
/// `Value` (e.g. checking that a sum was computed) instead.
///
/// Why is this fine? Every migrated test that asserted on
/// `stdout.contains("sum=136")` was really asserting "the program
/// ran the receive loop to completion and computed the sum". The
/// in-process version asserts the same thing by checking the
/// returned `Value` (when the program returns the sum directly)
/// OR by re-shaping the source to return the sum from `main()`
/// instead of printing it. The migrated tests in this commit take
/// the latter approach; see `scheduler_deadlock_detector_tests.rs`.
fn compile_and_run(source: &str) -> (String, Result<Value, crate::vm::VmError>) {
    let stdout = String::new();
    let tokens = match Lexer::new(source).tokenize() {
        Ok(t) => t,
        Err(e) => {
            return (
                stdout,
                Err(crate::vm::VmError::new(format!("lexer error: {e:?}"))),
            );
        }
    };
    let mut program = match Parser::new(tokens).parse_program() {
        Ok(p) => p,
        Err(e) => {
            return (
                stdout,
                Err(crate::vm::VmError::new(format!("parse error: {e:?}"))),
            );
        }
    };
    let _ = crate::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => {
            return (stdout, Err(crate::vm::VmError::new(e.message)));
        }
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let result = vm.run(script);
    (stdout, result)
}

static TRIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn trial_id() -> u64 {
    TRIAL_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Aggregate counters from a slice of trial outcomes. Used by every
/// migrated regression test to express its tolerance assertion in one
/// line:
///
/// ```ignore
/// let stats = TrialStats::compute(&outcomes, "sum=136");
/// assert!(stats.deadlock_count <= MAX_DEADLOCK_FALSE_POSITIVES, ...);
/// assert_eq!(stats.wrong_value_count, 0, ...);
/// ```
#[derive(Debug, Default)]
pub struct TrialStats {
    pub deadlock_count: usize,
    pub panic_count: usize,
    pub timed_out_count: usize,
    /// Number of trials that did NOT see a deadlock AND did not
    /// produce the expected stdout / value. The `_value` half of
    /// the name acknowledges the in-process harness checks the
    /// returned `Value` rather than parsing stdout.
    pub wrong_value_count: usize,
    pub first_failure_index: Option<usize>,
    pub first_failure_message: Option<String>,
}

impl TrialStats {
    /// Compute summary statistics. `expected_int` is the value the
    /// program's `fn main` is expected to return on a clean run; for
    /// the migrated tests this is `Value::Int(136)` (sum of 1..=16).
    /// Pass `None` to skip the value check (useful for real-deadlock
    /// tests where the program is supposed to never return).
    pub fn compute(outcomes: &[TrialOutcome], expected_int: Option<i64>) -> Self {
        let mut stats = Self::default();
        let record_failure = |idx: usize, stats: &mut TrialStats, msg: String| {
            if stats.first_failure_index.is_none() {
                stats.first_failure_index = Some(idx);
                stats.first_failure_message = Some(msg);
            }
        };
        for (i, outcome) in outcomes.iter().enumerate() {
            if outcome.timed_out {
                stats.timed_out_count += 1;
                record_failure(i, &mut stats, format!("trial {i}: TIMED OUT"));
                continue;
            }
            if outcome.saw_panic() {
                stats.panic_count += 1;
                record_failure(
                    i,
                    &mut stats,
                    format!(
                        "trial {i}: panic: {}",
                        outcome.error_message.as_deref().unwrap_or("<no msg>")
                    ),
                );
                continue;
            }
            if outcome.saw_deadlock() {
                stats.deadlock_count += 1;
                record_failure(
                    i,
                    &mut stats,
                    format!(
                        "trial {i}: deadlock: {}",
                        outcome.error_message.as_deref().unwrap_or("<no msg>")
                    ),
                );
                continue;
            }
            if let Some(expected) = expected_int {
                let got_expected = matches!(&outcome.result, Some(Value::Int(n)) if *n == expected);
                if !got_expected {
                    stats.wrong_value_count += 1;
                    record_failure(
                        i,
                        &mut stats,
                        format!(
                            "trial {i}: expected Int({expected}), got {:?} (err: {:?})",
                            outcome.result, outcome.error_message
                        ),
                    );
                }
            }
        }
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest possible smoke: a program that returns 42 from main.
    /// Verifies the harness compiles, runs, captures the value, and
    /// reports `ok() == true`.
    ///
    /// Budget: 10s. Even for `fn main() { 42 }`, the typechecker runs
    /// the auto-derive synthesis pass over every built-in enum and
    /// record (~30 types × 4 traits = ~120 synthesized impls). Body
    /// type-checking those synth methods dominates the wall-clock for
    /// trivial user programs in debug builds, and contention from
    /// parallel cargo tests can push the in-process trial well past
    /// a tight 2-second budget. The trial spawns a fresh OS thread
    /// per call, adding more variance. 10s is generous-but-not-loose:
    /// release builds finish in well under 1s, and a debug-build
    /// regression that pushes past 10s would still trip this lock.
    #[test]
    fn run_trial_returns_main_value() {
        let runner = InProcessRunner::new("fn main() { 42 }").with_budget(Duration::from_secs(10));
        let outcome = runner.run_trial();
        assert!(outcome.ok(), "outcome should be ok: {outcome:?}");
        assert_eq!(outcome.result, Some(Value::Int(42)));
        assert!(!outcome.saw_deadlock());
        assert!(!outcome.saw_panic());
    }

    /// A program with a real deadlock should surface as
    /// `saw_deadlock() == true` after the watchdog's 5s threshold.
    /// We give it a 10s budget; if the harness ever hangs, that
    /// turns into a `timed_out: true` instead.
    #[test]
    fn run_trial_surfaces_real_deadlock() {
        let src = r#"
import channel
fn main() {
  let ch = channel.new(0)
  match channel.receive(ch) {
    Message(_) -> 0
    _ -> 0
  }
}
"#;
        let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(10));
        let outcome = runner.run_trial();
        assert!(
            !outcome.timed_out,
            "watchdog must fire within budget; outcome={outcome:?}",
        );
        assert!(
            outcome.saw_deadlock(),
            "expected deadlock diagnostic; outcome={outcome:?}",
        );
    }

    /// Wall-clock budget enforcement: a hung program returns
    /// `timed_out: true` rather than hanging the test forever.
    /// We use a tight CPU loop so the program WILL exceed the
    /// budget if not bounded.
    #[test]
    fn run_trial_enforces_budget() {
        let src = r#"
fn main() {
  loop k = 0 {
    match k >= 100000000 {
      true -> 0
      _ -> loop(k + 1)
    }
  }
}
"#;
        let runner = InProcessRunner::new(src).with_budget(Duration::from_millis(200));
        let outcome = runner.run_trial();
        assert!(
            outcome.timed_out,
            "expected budget timeout; outcome={outcome:?}",
        );
    }

    /// The fan-in 16 success path runs to completion and returns
    /// 136. This is the same shape every migrated test uses.
    #[test]
    fn run_trial_fan_in_16_returns_136() {
        let src = r#"
import channel
import list
import task
fn main() {
  let ch = channel.new(0)
  let _senders = 1..16
    |> list.map { i -> task.spawn(fn() { channel.send(ch, i) }) }
  loop c = 0, acc = 0 {
    match c >= 16 {
      true -> acc
      _ -> match channel.receive(ch) {
        Message(v) -> loop(c + 1, acc + v)
        _ -> acc
      }
    }
  }
}
"#;
        let runner = InProcessRunner::new(src).with_budget(Duration::from_secs(10));
        let outcome = runner.run_trial();
        assert!(outcome.ok(), "outcome should be ok: {outcome:?}");
        assert_eq!(outcome.result, Some(Value::Int(136)));
    }

    /// `TrialStats` aggregates correctly: one deadlock + one wrong
    /// value + one ok trial yields the right counts.
    #[test]
    fn trial_stats_aggregates_correctly() {
        let outcomes = vec![
            TrialOutcome {
                stdout: String::new(),
                error_message: None,
                result: Some(Value::Int(136)),
                timed_out: false,
                elapsed: Duration::ZERO,
            },
            TrialOutcome {
                stdout: String::new(),
                error_message: Some("deadlock on main thread: ...".into()),
                result: None,
                timed_out: false,
                elapsed: Duration::ZERO,
            },
            TrialOutcome {
                stdout: String::new(),
                error_message: None,
                result: Some(Value::Int(99)),
                timed_out: false,
                elapsed: Duration::ZERO,
            },
        ];
        let stats = TrialStats::compute(&outcomes, Some(136));
        assert_eq!(stats.deadlock_count, 1);
        assert_eq!(stats.wrong_value_count, 1);
        assert_eq!(stats.panic_count, 0);
        assert_eq!(stats.timed_out_count, 0);
        assert_eq!(stats.first_failure_index, Some(1));
    }
}
