//! Regression tests for a scheduler race in `src/scheduler.rs`'s
//! `BlockReason::Send` / `BlockReason::Receive` arms.
//!
//! Bug (pre-fix) introduced by round-27's refactor to
//! `register_{send,recv}_waker_guard`: after registering the waker, the
//! scheduler cloned the task's handle out of `task_slot` for the cancel
//! cleanup closure:
//!
//! ```ignore
//! let reg = ch.register_send_waker_guard(Box::new(move || {
//!     if let Some(task) = slot.lock().take() { requeue(&inner2, task, false); }
//! }));
//! // ← waker may fire IMMEDIATELY if a peer is already parked on the
//! //   rendezvous handshake; firing sets task_slot to None.
//! let handle_for_cancel = task_slot.lock().as_ref()
//!     .expect("task_slot just initialized")   // ← panics: slot is None
//!     .handle.clone();
//! ```
//!
//! `register_{send,recv}_waker_guard` can synchronously invoke the
//! waker closure when a peer is already parked at a rendezvous (the
//! "fire inline during double-check" code path inside the channel's
//! register_* functions). The closure clears `task_slot`. The next
//! line then `expect`s the slot to still be `Some` → panic:
//! `thread '<unnamed>' panicked at src/scheduler.rs:576:30:
//! task_slot just initialized`.
//!
//! Fix: capture `handle_for_cancel` BEFORE calling
//! `register_*_waker_guard`. The slot is initialized immediately
//! above the `match reason { ... }` block and nothing mutates it
//! between there and the register call, so cloning the handle first
//! is safe.
//!
//! Repro: 16-sender fan-in on a rendezvous channel. The bug fired on
//! ~80% of debug runs, so N iterations gives detection probability
//! 1 − 0.2^N. N = 20 → ≥ 99.99%. We also mirror with 16 receivers /
//! 16 senders to stress the Receive arm's identical race.

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
        "silt_sched_race_{}_{}_{}_{}.silt",
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

/// Assert the run produced neither the scheduler panic fingerprint
/// nor any generic panic marker in stderr, and didn't time out.
///
/// Historical note: the 16-sender fan-in on a rendezvous channel used
/// to occasionally trigger silt's deadlock detector as a false positive
/// (main-thread receive checked `live > blocked` before any worker had
/// picked up the freshly-spawned senders). That race is closed by the
/// `pending_spawn` counter on `SchedulerInner` — see
/// `tests/scheduler_deadlock_detector_tests.rs`. Every trial here must
/// now reach `sum=136` with exit 0; there is no flake carve-out.
fn assert_no_scheduler_panic(trial: usize, label: &str, res: &RunResult) {
    assert!(
        !res.timed_out,
        "{label} trial {trial}: TIMEOUT; stdout={:?} stderr={:?}",
        res.stdout, res.stderr
    );
    assert!(
        !res.stderr.contains("task_slot just initialized"),
        "{label} trial {trial}: SCHEDULER RACE PANIC detected \
         ('task_slot just initialized'); stderr={}",
        res.stderr
    );
    assert!(
        !res.stderr.contains("thread '<unnamed>' panicked"),
        "{label} trial {trial}: unnamed-thread panic (scheduler worker) detected; stderr={}",
        res.stderr
    );
    assert!(
        !res.stderr.contains("thread 'main' panicked"),
        "{label} trial {trial}: main-thread panic detected; stderr={}",
        res.stderr
    );
    assert!(
        !res.stderr.contains("deadlock"),
        "{label} trial {trial}: deadlock-detector false positive; stderr={}",
        res.stderr
    );
}

/// **Send-arm race**: 16 senders fan in on a rendezvous channel; one
/// receiver drains all 16 values. When each sender blocks in
/// `BlockReason::Send`, `register_send_waker_guard` may fire inline
/// because the receiver is already parked at the handshake. The bug:
/// the handle was captured AFTER the register call, and the waker
/// closure (fired inline) had already cleared `task_slot`.
///
/// `1..16` is inclusive (16 values). Sum = 1+2+…+16 = 136.
///
/// Iterations: 20. Pre-fix failure rate is ~80% per run ⇒ per-20-run
/// detection probability ≥ 1 − 0.2^20 ≈ 1 − 1e-14 (effectively 100%).
/// In practice the bug fires within the first 1-2 trials on debug.
#[test]
fn test_send_arm_no_panic_16_sender_fan_in() {
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
    const ITERATIONS: usize = 20;
    for trial in 0..ITERATIONS {
        let res = run_silt(
            &format!("send_arm_fan_in_{trial}"),
            src,
            Duration::from_secs(15),
        );
        assert_no_scheduler_panic(trial, "send-arm fan-in", &res);
        assert_eq!(
            res.exit,
            Some(0),
            "send-arm fan-in trial {trial}: non-zero exit; \
             stdout={:?} stderr={:?}",
            res.stdout,
            res.stderr,
        );
        assert!(
            res.stdout.contains("sum=136"),
            "send-arm fan-in trial {trial}: expected sum=136; \
             stdout={:?} stderr={:?}",
            res.stdout,
            res.stderr,
        );
    }
}

/// **Receive-arm race**: symmetric shape. 16 receivers park on a
/// rendezvous channel; 16 senders then hand off values. When each
/// receiver blocks in `BlockReason::Receive`,
/// `register_recv_waker_guard` may fire inline because a sender is
/// already parked on the handshake (depending on interleaving), and
/// the pre-fix code cloned the handle after the register call.
///
/// Each receiver tells us its value via `task.join`; the main thread
/// sums them. Sum should be 136 again (1..16 inclusive).
#[test]
fn test_recv_arm_no_panic_16_receiver_fan_out() {
    let src = r#"
import channel
import list
import task

fn main() {
  let ch = channel.new(0)
  -- Park 16 receivers first so each sender's register_send may fire
  -- inline (matched sender path). Also stresses the receive-arm's
  -- register_recv_waker_guard: if a sender arrives first and parks
  -- at the rendezvous, a late receiver's register_recv can fire
  -- inline too.
  let receivers = 1..16
    |> list.map { _ -> task.spawn(fn() {
      match channel.receive(ch) {
        Message(v) -> v
        Closed -> 0
        Empty -> 0
        Sent -> 0
      }
    }) }
  let senders = 1..16
    |> list.map { i -> task.spawn(fn() { channel.send(ch, i) }) }
  -- Sum what the receivers saw. Trailing-lambda form:
  --   list.fold(init) { acc, elem -> ... }
  let sum = receivers
    |> list.map { h -> task.join(h) }
    |> list.fold(0) { acc, v -> acc + v }
  senders |> list.each { h -> task.join(h) }
  println("sum={sum}")
}
"#;
    const ITERATIONS: usize = 20;
    for trial in 0..ITERATIONS {
        let res = run_silt(
            &format!("recv_arm_fan_out_{trial}"),
            src,
            Duration::from_secs(15),
        );
        assert_no_scheduler_panic(trial, "recv-arm fan-out", &res);
        assert_eq!(
            res.exit,
            Some(0),
            "recv-arm fan-out trial {trial}: non-zero exit; \
             stdout={:?} stderr={:?}",
            res.stdout,
            res.stderr,
        );
        assert!(
            res.stdout.contains("sum=136"),
            "recv-arm fan-out trial {trial}: expected sum=136; \
             stdout={:?} stderr={:?}",
            res.stdout,
            res.stderr,
        );
    }
}
