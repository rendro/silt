//! Regression: the main-thread deadlock detector must not FALSE-fire on a
//! correct program under heavy CPU contention.
//!
//! Background: `is_main_starved` is evaluated against a single snapshot taken
//! under the wake-graph mutex (see `src/scheduler/wake_graph.rs` and
//! `src/builtins/concurrency.rs::main_thread_is_starved`). Under load that
//! snapshot can be *transiently* starved during a task state transition — e.g.
//! the window between a worker dequeuing a task and that task re-registering its
//! park edge, or between a task's last side effect and its `on_complete`
//! propagating. The pre-fix wait loops acted on that single snapshot and fired
//! "deadlock on main thread" immediately, so a genuinely-correct
//! supervisor/fan-in program could flake on a loaded CI runner. The fix gates a
//! suspected starvation behind a bounded confirmation wait on the progress
//! condvar (`confirm_main_starved`) before firing.
//!
//! These tests are PROBABILISTIC: a pure deterministic repro of a load race is
//! not feasible, so they spawn real work and run many iterations to maximise the
//! chance of hitting the transient window. They are pre-fix-failing (the pre-fix
//! detector intermittently false-fires on these shapes under contention) and
//! must pass post-fix. Every run must exit 0 with no "deadlock on main thread"
//! on stderr.

use std::process::Command;

/// Run a silt source via the `silt` CLI. Returns (ok, stdout, stderr).
fn run_silt_raw(source: &str) -> (bool, String, String) {
    let exe = env!("CARGO_BIN_EXE_silt");
    let mut tmp = std::env::temp_dir();
    let unique = format!(
        "silt_dl_false_pos_{}_{}.silt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    tmp.push(unique);
    std::fs::write(&tmp, source).expect("write temp silt source");

    let output = Command::new(exe)
        .arg("run")
        .arg(tmp.to_str().unwrap())
        .output()
        .expect("run silt binary");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_file(&tmp);
    (output.status.success(), stdout, stderr)
}

/// Run `source` `iterations` times; every run must exit 0 and never report a
/// main-thread deadlock.
fn assert_no_false_deadlock(label: &str, source: &str, iterations: usize) {
    for i in 0..iterations {
        let (ok, out, err) = run_silt_raw(source);
        assert!(
            !err.contains("deadlock on main thread"),
            "{label}: FALSE deadlock on iteration {i}; stderr={err:?} stdout={out:?}"
        );
        assert!(
            ok,
            "{label}: run failed on iteration {i}; stderr={err:?} stdout={out:?}"
        );
    }
}

/// Join fan-in: main spawns eight self-contained CPU workers and joins every
/// handle in turn. This drives the join wait loop (`main_thread_wait_for_join`)
/// — main parks on one live joinee at a time while the other workers churn the
/// scheduler. Each worker terminates on its own (no undrained channel that would
/// be a *real* deadlock), so any "no progress possible" diagnostic here is a
/// false positive: a joinee transiently starved in the dequeue ->
/// waker-registration window, which is exactly what the confirmation gate fixes.
const JOIN_FAN_IN: &str = r#"
import task

fn busy(id, n) {
  loop i = 0, acc = id {
    match i >= n {
      true -> acc
      _ -> loop(i + 1, acc + i)
    }
  }
}

fn main() {
  let h1 = task.spawn(fn() { busy(1, 400) })
  let h2 = task.spawn(fn() { busy(2, 400) })
  let h3 = task.spawn(fn() { busy(3, 400) })
  let h4 = task.spawn(fn() { busy(4, 400) })
  let h5 = task.spawn(fn() { busy(5, 400) })
  let h6 = task.spawn(fn() { busy(6, 400) })
  let h7 = task.spawn(fn() { busy(7, 400) })
  let h8 = task.spawn(fn() { busy(8, 400) })
  let _ = task.join(h1)
  let _ = task.join(h2)
  let _ = task.join(h3)
  let _ = task.join(h4)
  let _ = task.join(h5)
  let _ = task.join(h6)
  let _ = task.join(h7)
  let _ = task.join(h8)
  0
}
"#;

/// Direct fan-in: main spawns N senders, then receives all their values in a
/// loop. Each receive parks main on the channel while senders churn the
/// scheduler. This is the rendezvous shape from the prior round's bug report.
const DIRECT_FAN_IN: &str = r#"
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

#[test]
fn join_fan_in_no_false_deadlock_under_load() {
    assert_no_false_deadlock("join_fan_in", JOIN_FAN_IN, 25);
}

#[test]
fn direct_fan_in_no_false_deadlock_under_load() {
    assert_no_false_deadlock("direct_fan_in", DIRECT_FAN_IN, 25);
}
