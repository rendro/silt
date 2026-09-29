//! Regression tests for round-27 findings B1–B4: the scheduler's
//! `BlockReason::Receive` / `BlockReason::Send` cancel-cleanup path
//! takes the task slot but does NOT deregister the waker that the
//! BlockReason arm just registered on the channel. The `WakerId`
//! returned by `register_recv_waker` / `register_send_waker` is
//! silently discarded at the call site, leaving a dead waker and an
//! inflated `waiting_receivers` / `waiting_senders` counter on the
//! channel.
//!
//! Observable consequences (all four reproduced by the tests below):
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

use std::path::PathBuf;
use std::process::Command;
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

fn tmp_silt_file(stem: &str, src: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "silt_{}_{}_{}.silt",
        stem,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&tmp, src).unwrap();
    tmp
}

/// Run the silt binary on `src` with a wall-clock guard. Returns
/// (stdout, stderr, elapsed). Panics if the process takes longer than
/// `max_wall` — the deadlock scenarios we're testing otherwise hang.
fn run_silt(stem: &str, src: &str, max_wall: Duration) -> (String, String, Duration) {
    let tmp = tmp_silt_file(stem, src);
    let start = Instant::now();
    let output = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("failed to run silt binary");
    let elapsed = start.elapsed();
    let _ = std::fs::remove_file(&tmp);
    assert!(
        elapsed < max_wall,
        "silt run exceeded {max_wall:?} ({elapsed:?}); stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        elapsed,
    )
}

/// **B1**: Cancelled receiver leaks its recv-waker into the channel's
/// queue. A subsequent, unrelated `try_send` on that channel sees
/// `waiting_receivers > 0`, deposits into the handoff slot, and returns
/// `Sent` — even though no receiver is parked. Value lost.
///
/// Before the WakerRegistration guard: stdout is
/// `BUG:phantom_send_after_cancel`. After the fix: `ok`.
#[test]
fn test_b1_phantom_send_after_receiver_cancelled() {
    // Sleep durations bumped from 50ms → 200ms because Windows GitHub
    // runners have ~15.6ms timer granularity by default, so a 50ms
    // sleep often expires after one tick (~16ms) and cancel hasn't
    // had time to deregister the receiver before the probing
    // try_send fires. 200ms is safely past Windows' worst-case
    // tick boundary and the cancel waker-deregister path. Fast on
    // Linux/macOS; healthy headroom on Windows.
    let src = r#"
import channel
import task
import time

fn main() {
  let ch = channel.new(0)
  let h = task.spawn(fn() { channel.receive(ch) })
  time.sleep(time.ms(200))
  task.cancel(h)
  time.sleep(time.ms(200))
  -- No real receiver is parked on `ch`. A rendezvous `try_send` must
  -- return false. Before the fix, the cancelled receiver's leaked
  -- recv-waker keeps `waiting_receivers == 1`, and `try_send`
  -- phantom-succeeds (value silently dropped into the handoff slot).
  let probe = task.spawn(fn() { channel.try_send(ch, 999) })
  let ok = task.join(probe)
  match ok {
    true -> println("BUG:phantom_send_after_cancel")
    false -> println("ok")
    _ -> println("?")
  }
}
"#;
    let (stdout, stderr, _) = run_silt("b1_phantom_send", src, Duration::from_secs(15));
    assert!(
        stdout.contains("ok"),
        "expected 'ok' (try_send returns false — no phantom receiver); \
         got stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("BUG:"),
        "detected leaked recv-waker phantom send: stdout={stdout:?}"
    );
}

/// **B2**: Cancelled receiver's dead waker sits at the head of the
/// rendezvous channel's `recv_wakers` FIFO. A real receiver is queued
/// behind it. A later sender's `wake_recv` pops the dead waker
/// (no-op); the real receiver never wakes → deadlock detector fires.
///
/// Before the fix: the program deadlocks (the `task.join(real)` call
/// throws a deadlock error). After the fix: the real receiver gets
/// the value 7.
#[test]
fn test_b2_real_receiver_not_starved_by_cancelled_peer() {
    let src = r#"
import channel
import task
import time

fn main() {
  let ch = channel.new(0)
  -- First receiver: gets cancelled. Its waker stays at HEAD of FIFO.
  let dead = task.spawn(fn() {
    match channel.receive(ch) {
      Message(_v) -> 1
      _ -> -1
    }
  })
  time.sleep(time.ms(50))
  -- Second receiver: real, behind `dead` in FIFO.
  let real = task.spawn(fn() {
    match channel.receive(ch) {
      Message(v) -> v
      _ -> -1
    }
  })
  time.sleep(time.ms(50))
  task.cancel(dead)
  time.sleep(time.ms(50))
  -- Send a value. With the bug, wake_recv pops the dead waker (no-op)
  -- and `real` never wakes. With the fix, the dead waker was
  -- deregistered on cancel, so `real` is at the head and wakes
  -- normally.
  let snd = task.spawn(fn() {
    channel.send(ch, 7)
  })
  time.sleep(time.ms(50))
  let r = task.join(real)
  let _ = task.join(snd)
  println(r)
}
"#;
    let (stdout, stderr, _) = run_silt("b2_recv_starve", src, Duration::from_secs(15));
    assert!(
        stdout.contains("7"),
        "expected '7' (real receiver gets the value); \
         got stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("deadlock") && !stderr.contains("deadlock"),
        "detected starvation deadlock: stdout={stdout:?} stderr={stderr:?}"
    );
}

/// **B3**: Symmetric to B2 on the sender side. Cancelled sender's
/// waker stays at the head of `send_wakers`; a real sender queued
/// behind it never wakes when a receiver arrives.
#[test]
fn test_b3_real_sender_not_starved_by_cancelled_peer_rendezvous() {
    let src = r#"
import channel
import task
import time

fn main() {
  let ch = channel.new(0)
  -- First sender: will be cancelled. Its waker is HEAD of FIFO.
  let dead = task.spawn(fn() { channel.send(ch, 999) })
  time.sleep(time.ms(50))
  -- Second sender: real, behind `dead` in FIFO.
  let real = task.spawn(fn() {
    channel.send(ch, 7)
  })
  time.sleep(time.ms(50))
  task.cancel(dead)
  time.sleep(time.ms(50))
  -- Bring a receiver. With the bug, wake_send pops the dead waker
  -- first (no-op) and `real` never wakes. With the fix, the dead
  -- waker was deregistered on cancel, so `real` is at the head and
  -- handshakes normally.
  let recv = task.spawn(fn() {
    match channel.receive(ch) {
      Message(v) -> v
      _ -> -1
    }
  })
  let r = task.join(recv)
  let _ = task.join(real)
  println(r)
}
"#;
    let (stdout, stderr, _) = run_silt("b3_send_starve_rdv", src, Duration::from_secs(15));
    assert!(
        stdout.contains("7"),
        "expected '7' (receiver gets the real sender's value); \
         got stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("deadlock") && !stderr.contains("deadlock"),
        "detected sender-starvation deadlock: stdout={stdout:?} stderr={stderr:?}"
    );
}

/// **B4**: Same as B3 on a buffered channel (cap=1) after a drain.
/// The buffered channel is full; a first sender blocks and gets
/// cancelled; a second (real) sender queues behind. A subsequent
/// receive drains a slot — but `wake_send` pops the dead waker first,
/// leaving `real` parked forever.
///
/// The test joins on `real`; with the bug that join itself deadlocks.
#[test]
fn test_b4_real_sender_not_starved_by_cancelled_peer_buffered() {
    let src = r#"
import channel
import task
import time

fn main() {
  let ch = channel.new(1)
  channel.send(ch, 100)              -- fill the buffer
  let dead = task.spawn(fn() { channel.send(ch, 999) })   -- blocks
  time.sleep(time.ms(50))
  let real = task.spawn(fn() { channel.send(ch, 7) })     -- queued behind
  time.sleep(time.ms(50))
  task.cancel(dead)
  time.sleep(time.ms(50))
  -- Drain the initial value. With the bug, wake_send pops the dead
  -- waker (no-op) and `real` never wakes. With the fix, `real`
  -- wakes and its value lands in the buffer.
  match channel.receive(ch) {
    Message(_) -> ()
    _ -> ()
  }
  let _ = task.join(real)
  -- After `real` sent, its 7 must be in the buffer.
  match channel.receive(ch) {
    Message(v) -> println(v)
    _ -> println("CLOSED_OR_EMPTY")
  }
}
"#;
    let (stdout, stderr, _) = run_silt("b4_send_starve_buf", src, Duration::from_secs(15));
    assert!(
        stdout.contains("7"),
        "expected '7' (real sender delivered its value after drain); \
         got stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("deadlock") && !stderr.contains("deadlock"),
        "detected buffered sender-starvation deadlock: stdout={stdout:?} stderr={stderr:?}"
    );
}

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
