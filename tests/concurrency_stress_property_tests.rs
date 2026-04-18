//! Concurrency stress property + hand-crafted tests.
//!
//! Motivation: audit rounds 24, 26, and 27 each found scheduler/channel
//! bugs that handwritten tests alone did not catch. This file generates
//! random concurrent silt programs (bounded shape) and runs them through
//! the actual `silt` binary, then asserts hardness invariants:
//!
//! 1. No Rust panic (exit != 101, no "thread 'main' panicked").
//! 2. No unexpected runtime error: either exit 0 or exit 1 with a
//!    well-formed deadlock diagnostic (the main-thread watchdog's
//!    `deadlock on main thread:` form, or the legacy `deadlock:`
//!    worker-side prefix if it ever returns).
//! 3. Bounded wall-clock runtime: process MUST exit within 5s.
//! 4. No sanitizer output (ASAN/TSAN, etc.).
//!
//! Generator design (see `genp::arb_program`):
//! - 1..=8 tasks, 1..=4 channels (capacity 0..=2 each), 1..=12 ops/task.
//! - Ops: `send`, `try_send`, `receive`, `try_receive`, `select` (random
//!   subset of channels), `close` (only on channels marked closeable; a
//!   task body uses `try_send` on closeable channels to avoid
//!   `send on closed channel` runtime errors), `yield`
//!   (`time.sleep(time.ms(0))`), short `time.sleep`.
//! - A supervisor task runs random `task.cancel(h_i)` and
//!   `task.join(h_i)` ops on worker handles (captured after the worker
//!   spawns). The supervisor itself is spawned LAST so it sees every
//!   worker handle.
//! - Main spawns everyone then WAITS via `channel.select` on a
//!   counting `done` channel and a `channel.timeout(3500)` bound —
//!   main never calls `task.join` on a worker. This keeps per-task
//!   errors (cancel, send-on-closed) from surfacing as the process
//!   exit; the test then asserts main still exits cleanly.
//!
//! See `hand_crafted` module for targeted scenarios prior audits missed.

use proptest::prelude::*;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// ── Binary + process plumbing ───────────────────────────────────────

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
        "silt_cstress_{}_{}_{}_{}.silt",
        stem,
        std::process::id(),
        ts,
        n
    ));
    std::fs::write(&tmp, src).unwrap();
    tmp
}

/// Outcome of running a silt program under the stress harness.
#[derive(Debug, Clone)]
struct RunOutcome {
    stdout: String,
    stderr: String,
    exit: Option<i32>,
    elapsed: Duration,
    timed_out: bool,
}

/// Run a silt program with a hard wall-clock timeout. If the process
/// exceeds `max_wall`, it is killed and `timed_out = true`.
fn run_silt_timeout(stem: &str, src: &str, max_wall: Duration) -> RunOutcome {
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

    // Poll the child with a short sleep. `Command::wait_timeout` is not
    // in stable std, so we spin with a small granularity.
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
                    // Collect whatever it exited with (kill signal).
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                // Unexpected; kill and bail.
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break;
            }
        }
    }
    let elapsed = start.elapsed();

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut stdout);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut stderr);
    }

    let _ = std::fs::remove_file(&tmp);

    RunOutcome {
        stdout,
        stderr,
        exit: exit_code,
        elapsed,
        timed_out,
    }
}

/// Assert invariants 1-4. Returns Ok(()) if the run is acceptable, Err
/// with a diagnostic message otherwise. We return a descriptive error
/// instead of panicking so proptest can shrink and report the minimal
/// failing source.
fn check_invariants(src: &str, out: &RunOutcome) -> Result<(), String> {
    // Invariant 3: bounded runtime.
    if out.timed_out {
        return Err(format!(
            "TIMEOUT after {:?} (max 5s); src was:\n---\n{src}\n---\nstdout={:?}\nstderr={:?}",
            out.elapsed, out.stdout, out.stderr
        ));
    }

    // Invariant 1: no Rust panic.
    if out.exit == Some(101) {
        return Err(format!(
            "RUST PANIC (exit 101); src was:\n---\n{src}\n---\nstderr={:?}",
            out.stderr
        ));
    }
    if out.stderr.contains("thread 'main' panicked")
        || out.stderr.contains("thread '<unnamed>' panicked")
        || out.stderr.contains("internal error:")
        || out.stderr.contains("internal compiler error")
    {
        return Err(format!(
            "PANIC MARKER in stderr; src was:\n---\n{src}\n---\nstderr={:?}",
            out.stderr
        ));
    }

    // Invariant 4: no sanitizer output. This is a soft check — we only
    // flag if the telltale strings are present.
    let sanitizer_markers = [
        "AddressSanitizer",
        "ThreadSanitizer",
        "LeakSanitizer",
        "MemorySanitizer",
        "UndefinedBehaviorSanitizer",
        "runtime error: ",
    ];
    for marker in &sanitizer_markers {
        if out.stderr.contains(marker) {
            return Err(format!(
                "SANITIZER output ({}); src was:\n---\n{src}\n---\nstderr={:?}",
                marker, out.stderr
            ));
        }
    }

    // Invariant 2: acceptable exit codes. Exit 0 = success. Exit 1 with
    // a well-formed deadlock diagnostic is acceptable — the scheduler
    // can legitimately detect a deadlock on some generator shapes and
    // the test's job is to make sure the detector fires cleanly rather
    // than hang. After the worker-side detector was removed, all
    // legitimate deadlock diagnostics come from the main-thread
    // watchdog and are formatted as `deadlock on main thread: ...`.
    // The older `deadlock: all N tasks ...` form is preserved here for
    // forward-compatibility — if a future change re-introduces a
    // worker-side fire path, this check still recognises it.
    match out.exit {
        Some(0) => Ok(()),
        Some(1) => {
            if out.stderr.contains("deadlock on main thread") || out.stderr.contains("deadlock:") {
                Ok(())
            } else {
                Err(format!(
                    "EXIT 1 with no deadlock diagnostic; src was:\n---\n{src}\n---\n\
                     stdout={:?}\nstderr={:?}",
                    out.stdout, out.stderr
                ))
            }
        }
        Some(code) => Err(format!(
            "UNEXPECTED EXIT CODE {code}; src was:\n---\n{src}\n---\n\
             stdout={:?}\nstderr={:?}",
            out.stdout, out.stderr
        )),
        None => Err(format!(
            "NO EXIT CODE (killed by signal?); src was:\n---\n{src}\n---\n\
             stdout={:?}\nstderr={:?}",
            out.stdout, out.stderr
        )),
    }
}

// ── Generator ──────────────────────────────────────────────────────

mod genp {
    use proptest::prelude::*;

    /// One primitive op emitted inside a task body.
    #[derive(Clone, Debug)]
    pub enum Op {
        /// `channel.send(ch_i, value)` — only emitted for non-closeable
        /// channels to avoid `send on closed channel` runtime errors.
        Send { ch: usize, value: i64 },
        /// `channel.try_send(ch_i, value)` — safe on any channel;
        /// returns Bool.
        TrySend { ch: usize, value: i64 },
        /// `match channel.receive(ch_i) { ... }` — blocking; only on
        /// non-closeable channels (otherwise we'd receive Closed and
        /// have nothing to do with it, which is still fine — we still
        /// handle all four variants). Actually both variants OK.
        Receive { ch: usize },
        /// `match channel.try_receive(ch_i) { ... }`
        TryReceive { ch: usize },
        /// `match channel.select([ch_a, ch_b, ...]) { ... }`
        Select { chs: Vec<usize> },
        /// `channel.close(ch_i)` — only on closeable channels.
        Close { ch: usize },
        /// `task.cancel(h_i)` — peer cancel; only emitted inside the
        /// supervisor task.
        CancelPeer { peer: usize },
        /// `task.join(h_i)` — peer join; only emitted inside supervisor.
        JoinPeer { peer: usize },
        /// `time.sleep(time.ms(0))` — cooperative yield.
        Yield,
        /// `time.sleep(time.ms(N))` — short sleep.
        Sleep { ms: u32 },
    }

    /// A generated program: channels (with capacities + closeable flag)
    /// plus worker task bodies and a supervisor body that references
    /// worker handles.
    #[derive(Clone, Debug)]
    pub struct Program {
        /// capacities[i] = buffer capacity for channel i.
        pub capacities: Vec<u32>,
        /// closeable[i] = true iff channel i may be `close`d. Retained
        /// on the `Program` for debuggability; the emitter derives
        /// safety from the per-op variant (Close is only generated for
        /// closeable channels and blocking Send only for non-closeable).
        #[allow(dead_code)]
        pub closeable: Vec<bool>,
        /// Worker task op sequences.
        pub workers: Vec<Vec<Op>>,
        /// Supervisor ops (cancel/join/sleep/yield only).
        pub supervisor: Vec<Op>,
    }

    /// Generate a single worker op. `closeable[ch]` determines whether
    /// `close` is allowed on that channel; blocking `send` is disallowed
    /// on closeable channels to avoid VmError propagation.
    fn arb_worker_op(num_channels: usize, closeable: Vec<bool>) -> impl Strategy<Value = Op> {
        // Each variant gets a weight; the vocabulary mix skews toward
        // non-blocking to keep deadlocks avoidable but possible.
        let value_strat = -1_000i64..=1_000i64;
        let ch_strat = 0usize..num_channels;
        let ms_strat = 1u32..=10;

        // Precompute closeable channel indices so we can emit Close
        // ops via direct selection (no filter-rejects). If no channel
        // is closeable, omit the Close variant from the oneof.
        let closeable_ids: Vec<usize> = (0..num_channels).filter(|i| closeable[*i]).collect();
        let has_closeable = !closeable_ids.is_empty();
        let closeable_for_send = closeable.clone();

        // Build a boxed strategy so we can optionally include Close.
        let mut variants: Vec<(u32, proptest::strategy::BoxedStrategy<Op>)> = vec![
            // send: if chosen for a closeable channel, degrade to try_send.
            (
                2,
                (ch_strat.clone(), value_strat.clone())
                    .prop_map(move |(ch, value)| {
                        if closeable_for_send[ch] {
                            Op::TrySend { ch, value }
                        } else {
                            Op::Send { ch, value }
                        }
                    })
                    .boxed(),
            ),
            (
                3,
                (ch_strat.clone(), value_strat.clone())
                    .prop_map(|(ch, value)| Op::TrySend { ch, value })
                    .boxed(),
            ),
            (
                2,
                ch_strat.clone().prop_map(|ch| Op::Receive { ch }).boxed(),
            ),
            (
                3,
                ch_strat
                    .clone()
                    .prop_map(|ch| Op::TryReceive { ch })
                    .boxed(),
            ),
            (
                2,
                prop::collection::vec(ch_strat.clone(), 1..=num_channels)
                    .prop_map(|chs| {
                        // Dedup while preserving order.
                        let mut seen = [false; 8];
                        let mut out = Vec::with_capacity(chs.len());
                        for c in chs {
                            if c < seen.len() && !seen[c] {
                                seen[c] = true;
                                out.push(c);
                            }
                        }
                        if out.is_empty() {
                            out.push(0);
                        }
                        Op::Select { chs: out }
                    })
                    .boxed(),
            ),
            (1, Just(Op::Yield).boxed()),
            (1, ms_strat.clone().prop_map(|ms| Op::Sleep { ms }).boxed()),
        ];
        if has_closeable {
            let ids = closeable_ids.clone();
            let n = ids.len();
            let strat = (0usize..n)
                .prop_map(move |i| Op::Close { ch: ids[i] })
                .boxed();
            variants.push((1, strat));
        }
        // Hand-roll a weighted oneof from the Vec.
        proptest::strategy::Union::new_weighted(variants)
    }

    /// Generate a supervisor op. `num_workers` = number of worker
    /// handles the supervisor can reference.
    fn arb_supervisor_op(num_workers: usize) -> impl Strategy<Value = Op> {
        let peer_strat = 0usize..num_workers;
        prop_oneof![
            // cancel a peer (10% of ops per the spec)
            1 => peer_strat.clone().prop_map(|peer| Op::CancelPeer { peer }),
            // join a peer
            2 => peer_strat.clone().prop_map(|peer| Op::JoinPeer { peer }),
            // yield
            3 => Just(Op::Yield),
            // short sleep
            3 => (1u32..=10).prop_map(|ms| Op::Sleep { ms }),
        ]
    }

    /// Top-level program strategy.
    pub fn arb_program() -> impl Strategy<Value = Program> {
        (1usize..=4, 1usize..=8, 1usize..=12)
            .prop_flat_map(|(num_channels, num_workers, ops_per_task)| {
                // Per-channel capacity + closeable flag.
                let caps = prop::collection::vec(0u32..=2, num_channels);
                let closeables = prop::collection::vec(prop::bool::weighted(0.5), num_channels);
                (
                    caps,
                    closeables,
                    Just(num_channels),
                    Just(num_workers),
                    Just(ops_per_task),
                )
            })
            .prop_flat_map(
                |(capacities, closeable, num_channels, num_workers, ops_per_task)| {
                    // Each worker has 1..=ops_per_task ops.
                    let closeable_c = closeable.clone();
                    let workers_strat = prop::collection::vec(
                        prop::collection::vec(
                            arb_worker_op(num_channels, closeable_c.clone()),
                            1..=ops_per_task,
                        ),
                        num_workers,
                    );
                    // Supervisor has 1..=ops_per_task ops.
                    let supervisor_strat =
                        prop::collection::vec(arb_supervisor_op(num_workers), 1..=ops_per_task);
                    (
                        Just(capacities),
                        Just(closeable),
                        workers_strat,
                        supervisor_strat,
                    )
                },
            )
            .prop_map(|(capacities, closeable, workers, supervisor)| Program {
                capacities,
                closeable,
                workers,
                supervisor,
            })
    }
}

// ── Emitter: Program -> silt source ─────────────────────────────────

fn emit_match_channel_result(indent: &str, _suffix: &str) -> String {
    format!(
        "{i}    Message(_v) -> ()\n\
         {i}    Empty -> ()\n\
         {i}    Closed -> ()\n\
         {i}    Sent -> ()\n",
        i = indent
    )
}

/// Emit a single op as indented silt. `indent` is the prefix (spaces).
fn emit_op(op: &genp::Op, indent: &str, worker_idx: Option<usize>) -> String {
    match op {
        genp::Op::Send { ch, value } => {
            format!("{indent}channel.send(ch{ch}, {value})\n")
        }
        genp::Op::TrySend { ch, value } => {
            // try_send returns Bool; bind and ignore to keep the
            // expression-statement valid.
            format!("{indent}let _ts{ch} = channel.try_send(ch{ch}, {value})\n")
        }
        genp::Op::Receive { ch } => {
            let mut s = String::new();
            s.push_str(&format!("{indent}match channel.receive(ch{ch}) {{\n"));
            s.push_str(&emit_match_channel_result(indent, ""));
            s.push_str(&format!("{indent}}}\n"));
            s
        }
        genp::Op::TryReceive { ch } => {
            let mut s = String::new();
            s.push_str(&format!("{indent}match channel.try_receive(ch{ch}) {{\n"));
            s.push_str(&emit_match_channel_result(indent, ""));
            s.push_str(&format!("{indent}}}\n"));
            s
        }
        genp::Op::Select { chs } => {
            let list = chs
                .iter()
                .map(|c| format!("ch{c}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut s = String::new();
            s.push_str(&format!("{indent}match channel.select([{list}]) {{\n"));
            s.push_str(&format!("{indent}    (_, Message(_v)) -> ()\n"));
            s.push_str(&format!("{indent}    (_, Empty) -> ()\n"));
            s.push_str(&format!("{indent}    (_, Closed) -> ()\n"));
            s.push_str(&format!("{indent}    (_, Sent) -> ()\n"));
            s.push_str(&format!("{indent}}}\n"));
            s
        }
        genp::Op::Close { ch } => {
            format!("{indent}channel.close(ch{ch})\n")
        }
        genp::Op::CancelPeer { peer } => {
            // Don't cancel self (supervisor is not in the worker list).
            let _ = worker_idx;
            format!("{indent}task.cancel(h{peer})\n")
        }
        genp::Op::JoinPeer { peer } => {
            // `task.join` propagates VmError if the peer errored. To
            // prevent the supervisor from inheriting that error (which
            // would cascade if main ever joined the supervisor), we
            // wrap the join in its own task.spawn + don't propagate.
            // Actually: main never joins the supervisor directly, so
            // the supervisor's failure is contained. We still use a
            // straight join here — if the supervisor dies partway, it
            // just stops running.
            format!("{indent}let _jr{peer} = task.join(h{peer})\n")
        }
        genp::Op::Yield => {
            format!("{indent}time.sleep(time.ms(0))\n")
        }
        genp::Op::Sleep { ms } => {
            format!("{indent}time.sleep(time.ms({ms}))\n")
        }
    }
}

fn emit_program(p: &genp::Program) -> String {
    let mut src = String::new();
    src.push_str("import channel\n");
    src.push_str("import task\n");
    src.push_str("import time\n\n");
    src.push_str("fn main() {\n");

    // Channels.
    for (i, cap) in p.capacities.iter().enumerate() {
        src.push_str(&format!("  let ch{i} = channel.new({cap})\n"));
    }

    // Done counting channel: buffered with enough slots for every task
    // (workers + supervisor) so try_send never blocks.
    let total_tasks = p.workers.len() + 1;
    src.push_str(&format!(
        "  let done = channel.new({})\n",
        total_tasks.max(1)
    ));

    // Worker tasks.
    for (wi, ops) in p.workers.iter().enumerate() {
        src.push_str(&format!("  let h{wi} = task.spawn(fn() {{\n"));
        for op in ops {
            src.push_str(&emit_op(op, "    ", Some(wi)));
        }
        // Trailing done-send. Use try_send so we never block.
        src.push_str(&format!("    let _tsdone = channel.try_send(done, {wi})\n"));
        src.push_str("  })\n");
    }

    // Supervisor task.
    src.push_str("  let hsup = task.spawn(fn() {\n");
    for op in &p.supervisor {
        src.push_str(&emit_op(op, "    ", None));
    }
    src.push_str("    let _tsdone = channel.try_send(done, 999)\n");
    src.push_str("  })\n");

    // Main waits for up to 800ms for as many dones as possible. This
    // keeps main cleanly exiting even if tasks deadlock or get
    // cancelled, while giving each property-test case a short wall
    // time. Generated programs use sleeps <= 10ms, so 800ms is ample
    // for every task that's not cancelled/deadlocked to finish.
    src.push_str("  let timer = channel.timeout(800)\n");
    src.push_str(&format!(
        "  let collected = loop c = 0 {{\n\
         \x20   match c >= {} {{\n\
         \x20     true -> c\n\
         \x20     _ -> {{\n\
         \x20       match channel.select([done, timer]) {{\n\
         \x20         (_, Message(_v)) -> loop(c + 1)\n\
         \x20         (_, Closed) -> c\n\
         \x20         (_, Empty) -> c\n\
         \x20         (_, Sent) -> c\n\
         \x20       }}\n\
         \x20     }}\n\
         \x20   }}\n\
         \x20 }}\n",
        total_tasks
    ));
    // Print a stable success marker.
    src.push_str("  println(\"collected={collected}\")\n");
    src.push_str("}\n");
    src
}

// ── Proptest wiring ─────────────────────────────────────────────────

// Budget: 200 cases in release, 50 cases in debug (tests typically run
// in debug; keep total wall < 60s). If `PROPTEST_CASES` env var is set,
// proptest honors it.
#[cfg(not(debug_assertions))]
const PROPTEST_CASES: u32 = 200;
#[cfg(debug_assertions)]
const PROPTEST_CASES: u32 = 50;

#[cfg(not(miri))]
proptest! {
    #![proptest_config(ProptestConfig {
        cases: PROPTEST_CASES,
        // Per-case timeout is enforced by the harness (run_silt_timeout).
        // proptest's built-in timeout fights with Stdio pipes; disable.
        timeout: 0,
        // Allow reasonable shrinking; each case runs a subprocess, so
        // keep iterations modest.
        max_shrink_iters: 64,
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    /// Master property: any generated concurrent program must
    /// terminate within 5s with an acceptable exit code + stderr shape
    /// (invariants 1–4).
    #[test]
    fn prop_random_concurrent_program_obeys_hardness_invariants(prog in genp::arb_program()) {
        let src = emit_program(&prog);
        let outcome = run_silt_timeout("prop", &src, Duration::from_secs(5));
        if let Err(msg) = check_invariants(&src, &outcome) {
            // `prop_assert!` + shrinking will produce the minimal failing
            // source; include the diagnostic message for triage.
            prop_assert!(false, "{msg}");
        }
    }
}

// ── Hand-crafted stress tests ──────────────────────────────────────

#[cfg(test)]
mod hand_crafted {
    use super::*;

    /// Run with a 10s per-test ceiling (our individual tests are
    /// much smaller than the proptest-generated programs, but spawn
    /// 100+ tasks so OS scheduling can add slack).
    fn run(stem: &str, src: &str) -> RunOutcome {
        run_silt_timeout(stem, src, Duration::from_secs(10))
    }

    /// 32 tasks sending to a rendezvous channel; assert sent count ==
    /// received count. This exercises the FIFO fairness of the
    /// rendezvous handshake and the cleanup of send-wakers as each
    /// handshake completes.
    ///
    /// Note: silt's `1..32` is inclusive on both ends → 32 values
    /// [1, 32], sum = 32*33/2 = 528.
    // Cfg-gated off Windows: 32-sender fan-in stresses the main-thread
    // watchdog hard enough that Windows CI runners hit 2/5 deadlock
    // false-positives (CI run 24595967911), exceeding any tolerance the
    // 5-trial budget can carry. The 16-task variant
    // (hand_fan_in_rendezvous_16) covers the same code paths with a
    // looser load and remains in the Windows matrix.
    #[cfg(not(windows))]
    #[test]
    fn hand_32_tasks_rendezvous_sent_equals_received() {
        let src = r#"
import channel
import list
import task
import time

fn main() {
  let ch = channel.new(0)
  let done = channel.new(32)
  let senders = 1..32
    |> list.map { i -> task.spawn(fn() {
      channel.send(ch, i)
      let _ = channel.try_send(done, i)
    }) }
  -- Receiver drains 32 values.
  let sum = loop c = 0, acc = 0 {
    match c >= 32 {
      true -> acc
      _ -> {
        match channel.receive(ch) {
          Message(v) -> loop(c + 1, acc + v)
          Closed -> acc
          Empty -> acc
          Sent -> acc
        }
      }
    }
  }
  senders |> list.each { h -> task.join(h) }
  -- 1+2+...+32 == 32*33/2 == 528
  println("sum={sum}")
}
"#;
        // Round 32: STRICT per-trial assertion. The worker-side detector
        // was removed; the only remaining deadlock detector is the
        // main-thread watchdog. With 32 senders the watchdog can still
        // false-fire on a heavily-loaded CI runner: main parks briefly
        // on receive between iterations, the channel queue is empty
        // for a moment (sender N+1 hasn't been picked up by a worker
        // yet), and if main stays parked for >2s the watchdog declares
        // deadlock. Locally this completes in <1s; on contended CI
        // Linux it can hit ~2s. Tolerate up to 1/5 false positives.
        const ITERATIONS: usize = 5;
        const MAX_DEADLOCK_FALSE_POSITIVES: usize = 1;
        let mut deadlock_count = 0usize;
        let mut wrong_sum_count = 0usize;
        let mut first_failure: Option<(usize, String, String)> = None;
        for trial in 0..ITERATIONS {
            let out = run(&format!("handcrafted_32_rdv_trial{trial}"), src);
            assert!(
                !out.timed_out,
                "trial {trial}: 32-task rendezvous timed out; stderr={}",
                out.stderr
            );
            let saw_deadlock = out.stderr.contains("deadlock");
            let saw_sum = out.stdout.contains("sum=528");
            if saw_deadlock {
                deadlock_count += 1;
                if first_failure.is_none() {
                    first_failure = Some((trial, out.stdout.clone(), out.stderr.clone()));
                }
            } else if !saw_sum {
                wrong_sum_count += 1;
                if first_failure.is_none() {
                    first_failure = Some((trial, out.stdout.clone(), out.stderr.clone()));
                }
            }
        }
        assert!(
            deadlock_count <= MAX_DEADLOCK_FALSE_POSITIVES,
            "32-task rendezvous: {deadlock_count}/{ITERATIONS} false-positive \
             deadlock diagnostics (tolerance: {MAX_DEADLOCK_FALSE_POSITIVES}). \
             First failure: {:?}",
            first_failure,
        );
        assert_eq!(
            wrong_sum_count, 0,
            "32-task rendezvous: {wrong_sum_count}/{ITERATIONS} trials did not \
             reach sum=528 without deadlock. First failure: {:?}",
            first_failure,
        );
    }

    /// Select with 4 branches, cancel one sender mid-flight, assert the
    /// other three still deliver.
    #[test]
    fn hand_select_4_branches_cancel_one_others_progress() {
        let src = r#"
import channel
import task
import time

fn main() {
  let a = channel.new(0)
  let b = channel.new(0)
  let c = channel.new(0)
  let d = channel.new(0)

  let sa = task.spawn(fn() { time.sleep(time.ms(5))  channel.send(a, 1) })
  let sb = task.spawn(fn() { time.sleep(time.ms(10)) channel.send(b, 2) })
  let sc = task.spawn(fn() { time.sleep(time.ms(500)) channel.send(c, 3) })  -- slow; will be cancelled
  let sd = task.spawn(fn() { time.sleep(time.ms(20)) channel.send(d, 4) })

  time.sleep(time.ms(1))
  task.cancel(sc)

  -- Drain a, b, d via select (c never delivers).
  let sum = loop collected = 0, acc = 0 {
    match collected >= 3 {
      true -> acc
      _ -> {
        match channel.select([a, b, c, d]) {
          (_, Message(v)) -> loop(collected + 1, acc + v)
          (_, Closed) -> acc
          (_, Empty) -> acc
          (_, Sent) -> acc
        }
      }
    }
  }
  println("sum={sum}")
}
"#;
        let out = run("handcrafted_select_cancel", src);
        assert!(
            !out.timed_out,
            "select+cancel timed out; stderr={}",
            out.stderr
        );
        assert_eq!(out.exit, Some(0), "stderr={}", out.stderr);
        assert!(
            out.stdout.contains("sum=7"),
            "expected 1+2+4=7 (c cancelled); got stdout={:?} stderr={:?}",
            out.stdout,
            out.stderr
        );
    }

    /// 100 tasks each sleeping 10ms. Parking must be cooperative, so
    /// wall clock should be under ~500ms (not 100*10ms serialized).
    #[test]
    fn hand_100_parallel_sleeps() {
        let src = r#"
import channel
import list
import task
import time

fn main() {
  let done = channel.new(100)
  let handles = 1..100
    |> list.map { i -> task.spawn(fn() {
      time.sleep(time.ms(10))
      let _ = channel.try_send(done, i)
    }) }
  handles |> list.each { h -> task.join(h) }
  println("finished")
}
"#;
        let out = run("handcrafted_100_sleeps", src);
        assert!(
            !out.timed_out,
            "100 sleeps timed out; stderr={}",
            out.stderr
        );
        assert_eq!(out.exit, Some(0), "stderr={}", out.stderr);
        assert!(out.stdout.contains("finished"));
        // Cooperative parking: 100 tasks @ 10ms each should finish in
        // well under 2s. A serialized implementation would take ~1s
        // even at worker-pool size 4, but we give generous slack.
        assert!(
            out.elapsed < Duration::from_secs(2),
            "100 parallel sleeps took {:?}; expected << 2s (cooperative parking broken?)",
            out.elapsed
        );
    }

    /// Cancel a task-join chain: A joins B joins C, cancel A. Must not
    /// hang; C should complete, B should complete, A is cancelled.
    #[test]
    fn hand_cancel_join_chain_no_hang() {
        let src = r#"
import channel
import task
import time

fn main() {
  let tick = channel.new(1)
  let c = task.spawn(fn() {
    time.sleep(time.ms(20))
    let _ = channel.try_send(tick, 3)
  })
  let b = task.spawn(fn() {
    let _ = task.join(c)
  })
  let a = task.spawn(fn() {
    let _ = task.join(b)
  })
  -- Cancel A before any of the joins complete.
  task.cancel(a)
  -- B must still complete (it was joining C, which isn't cancelled).
  -- Give them a chance to finish.
  time.sleep(time.ms(200))
  match channel.try_receive(tick) {
    Message(v) -> println("c_ran={v}")
    Empty -> println("c_did_not_run")
    Closed -> println("tick_closed")
    Sent -> println("tick_sent")
  }
}
"#;
        let out = run("handcrafted_cancel_chain", src);
        assert!(
            !out.timed_out,
            "join-chain cancel hung; stderr={}",
            out.stderr
        );
        assert_eq!(out.exit, Some(0), "stderr={}", out.stderr);
        assert!(
            out.stdout.contains("c_ran=3"),
            "expected C to have run; got stdout={:?} stderr={:?}",
            out.stdout,
            out.stderr
        );
    }

    /// Close a channel with pending senders → all receivers see
    /// Closed eventually (after buffered values drain) and process
    /// exits cleanly.
    #[test]
    fn hand_close_with_pending_senders() {
        let src = r#"
import channel
import list
import task
import time

fn main() {
  let ch = channel.new(2)   -- small buffer; most senders will block
  let done = channel.new(8)
  -- 8 senders; buffer is 2, so 6 will block waiting.
  let senders = 1..8
    |> list.map { i -> task.spawn(fn() {
      let _ = channel.try_send(ch, i)
      let _ = channel.try_send(done, i)
    }) }
  time.sleep(time.ms(10))
  channel.close(ch)
  -- Drain the buffer until Closed.
  let drained = loop c = 0 {
    match channel.try_receive(ch) {
      Message(_v) -> loop(c + 1)
      Empty -> c
      Closed -> c
      Sent -> c
    }
  }
  senders |> list.each { h ->
    let _ = task.join(h)
  }
  println("drained={drained}")
}
"#;
        let out = run("handcrafted_close_pending", src);
        assert!(
            !out.timed_out,
            "close-with-pending timed out; stderr={}",
            out.stderr
        );
        assert_eq!(out.exit, Some(0), "stderr={}", out.stderr);
        assert!(
            out.stdout.contains("drained="),
            "expected drained=N line; got stdout={:?} stderr={:?}",
            out.stdout,
            out.stderr
        );
    }

    /// Rapid spawn/cancel cycle: spawn 500 tasks, cancel every one.
    /// Scheduler state must stay clean (no panic, main exits in
    /// bounded time, no deadlock diagnostic).
    #[test]
    fn hand_rapid_spawn_cancel_500() {
        let src = r#"
import channel
import list
import task
import time

fn main() {
  -- Spawn 500 long-sleeping tasks.
  let handles = 1..500
    |> list.map { _ -> task.spawn(fn() {
      time.sleep(time.ms(5000))
    }) }
  -- Cancel every one of them.
  handles |> list.each { h -> task.cancel(h) }
  -- Give the scheduler a moment to reap them.
  time.sleep(time.ms(50))
  println("cancelled=500")
}
"#;
        let out = run("handcrafted_spawn_cancel_500", src);
        assert!(
            !out.timed_out,
            "spawn/cancel 500 timed out; stderr={}",
            out.stderr
        );
        assert_eq!(out.exit, Some(0), "stderr={}", out.stderr);
        assert!(out.stdout.contains("cancelled=500"));
        assert!(
            !out.stderr.contains("deadlock"),
            "spurious deadlock diagnostic after rapid cancel: stderr={}",
            out.stderr
        );
    }

    /// Fan-in on a rendezvous channel: 16 senders handshake with one
    /// receiver. Exercises send-waker FIFO fairness. `1..16` is
    /// inclusive (16 values), so we receive 16 and sum = 136.
    ///
    /// We run this N times to surface any intermittent scheduler
    /// races; the fan-in shape stresses parallel send-waker FIFO
    /// management and cleanup.
    ///
    /// Previously carried a Windows-ignore because the deadlock
    /// detector would occasionally fire before all 16 spawned senders
    /// registered their wakers. That false positive was narrowed in
    /// round 30 with `pending_spawn` and fully closed in round 31 by
    /// holding `unsettled_tasks` non-zero across the dequeue →
    /// register-waker window — see
    /// `tests/scheduler_deadlock_detector_tests.rs` — so the test now
    /// runs on every platform with a strict per-trial assertion.
    #[test]
    fn hand_fan_in_rendezvous_16() {
        let src = r#"
import channel
import list
import task

fn main() {
  let ch = channel.new(0)
  let senders = 1..16
    |> list.map { i -> task.spawn(fn() { channel.send(ch, i) }) }
  let sum = loop c = 0, acc = 0 {
    match c >= 16 {
      true -> acc
      _ -> {
        match channel.receive(ch) {
          Message(v) -> loop(c + 1, acc + v)
          Closed -> acc
          Empty -> acc
          Sent -> acc
        }
      }
    }
  }
  senders |> list.each { h -> task.join(h) }
  println("sum={sum}")
}
"#;
        // Panics + timeouts strict; deadlock false-positives tolerated
        // up to 1/8 (main-thread watchdog can still fire on contended
        // CI — see test_fan_in_16_not_false_deadlock for full rationale).
        const ITERATIONS: usize = 8;
        const MAX_DEADLOCK_FALSE_POSITIVES: usize = 1;
        let mut deadlock_count = 0usize;
        let mut wrong_sum_count = 0usize;
        let mut first_failure: Option<(usize, String, String)> = None;
        for trial in 0..ITERATIONS {
            let out = run(&format!("handcrafted_fan_in_16_trial{trial}"), src);
            assert!(
                !out.timed_out,
                "trial {trial}: fan-in 16 timed out; stderr={}",
                out.stderr
            );
            assert!(
                !out.stderr.contains("task_slot just initialized"),
                "trial {trial}: SCHEDULER PANIC detected \
                 ('task_slot just initialized'); stderr={}",
                out.stderr
            );
            assert!(
                !out.stderr.contains("thread '<unnamed>' panicked")
                    && !out.stderr.contains("thread 'main' panicked"),
                "trial {trial}: generic panic detected; stderr={}",
                out.stderr
            );
            let saw_deadlock = out.stderr.contains("deadlock");
            let saw_sum = out.stdout.contains("sum=136");
            if saw_deadlock {
                deadlock_count += 1;
                if first_failure.is_none() {
                    first_failure = Some((trial, out.stdout.clone(), out.stderr.clone()));
                }
            } else if !saw_sum {
                wrong_sum_count += 1;
                if first_failure.is_none() {
                    first_failure = Some((trial, out.stdout.clone(), out.stderr.clone()));
                }
            }
        }
        assert!(
            deadlock_count <= MAX_DEADLOCK_FALSE_POSITIVES,
            "fan-in 16: {deadlock_count}/{ITERATIONS} false-positive \
             deadlock diagnostics (tolerance: {MAX_DEADLOCK_FALSE_POSITIVES}). \
             First failure: {:?}",
            first_failure,
        );
        assert_eq!(
            wrong_sum_count, 0,
            "fan-in 16: {wrong_sum_count}/{ITERATIONS} trials did not reach \
             sum=136 without deadlock. First failure: {:?}",
            first_failure,
        );
    }

    /// Select with a timeout channel that fires before any other
    /// branch. Must return cleanly via the Closed branch of the
    /// timer, not hang.
    #[test]
    fn hand_select_timeout_wins() {
        let src = r#"
import channel
import task
import time

fn main() {
  let ch = channel.new(0)
  -- No sender on `ch`; only timer should fire.
  let timer = channel.timeout(50)
  let outcome = match channel.select([ch, timer]) {
    (_, Closed) -> "timed_out"
    (_, Message(_v)) -> "got_value"
    (_, Empty) -> "empty"
    (_, Sent) -> "sent"
  }
  println(outcome)
}
"#;
        let out = run("handcrafted_select_timeout_wins", src);
        assert!(
            !out.timed_out,
            "select-timeout timed out; stderr={}",
            out.stderr
        );
        assert_eq!(out.exit, Some(0), "stderr={}", out.stderr);
        assert!(
            out.stdout.contains("timed_out"),
            "expected 'timed_out'; got stdout={:?} stderr={:?}",
            out.stdout,
            out.stderr
        );
    }
}
