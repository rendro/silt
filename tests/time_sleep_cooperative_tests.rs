//! Integration tests for `time.sleep` cooperative parking.
//!
//! `time.sleep` must park the scheduled task on the shared timer thread
//! rather than blocking the worker thread with `thread::sleep`. When
//! sixteen tasks each sleep 500ms, wall time should be ~500ms (not
//! serialized across the 4-ish worker pool).

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn silt_bin() -> PathBuf {
    if let Some(p) = std::env::var("CARGO_BIN_EXE_silt").ok() {
        return PathBuf::from(p);
    }
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps/
    if p.ends_with("deps") {
        p.pop();
    }
    p.push("silt");
    p
}

fn run_silt(src: &str) -> (String, String, i32, Duration) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("silt_sleep_{}_{n}.silt", std::process::id()));
    std::fs::write(&tmp, src).unwrap();
    let start = Instant::now();
    let output = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .output()
        .unwrap();
    let wall = start.elapsed();
    let _ = std::fs::remove_file(&tmp);
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
        wall,
    )
}

#[test]
fn test_time_sleep_parks_cooperatively_n_tasks_run_in_parallel() {
    // Spawn 16 tasks each sleeping 500ms. If `time.sleep` parks
    // cooperatively via the shared timer thread, wall time ≈ 500ms. If
    // it still blocks the worker thread, wall time ≈
    // ceil(16 / n_workers) * 500ms (on the default 4-worker pool, ~2s;
    // with the old 1ms-busy-loop impl, much worse due to scheduler
    // stalls).
    let src = r#"
import list
import task
import time

fn main() {
  let handles = 1..16
    |> list.map { _ -> task.spawn(fn() { time.sleep(time.ms(500)) }) }
  handles |> list.each { h -> task.join(h) }
  println("done")
}
"#;
    let (stdout, stderr, code, wall) = run_silt(src);
    assert_eq!(code, 0, "silt exit nonzero; stderr={stderr}");
    assert!(
        stdout.contains("done"),
        "expected 'done' in stdout; got {stdout:?}"
    );
    eprintln!(
        "test_time_sleep_parks_cooperatively_n_tasks_run_in_parallel: wall={:?}",
        wall
    );
    assert!(
        wall < Duration::from_millis(2000),
        "16 parallel 500ms sleeps took {wall:?}; expected well under 2s (cooperative park)"
    );
}

#[test]
fn test_time_sleep_returns_unit_after_delay_at_least_n_ms() {
    // Single task sleeps 100ms. Confirms the delay still actually
    // happens (guards against accidentally completing the
    // IoCompletion instantly).
    let src = r#"
import task
import test
import time

fn main() {
  let h = task.spawn(fn() {
    let before = time.now()
    time.sleep(time.ms(100))
    let elapsed = time.since(before, time.now())
    test.assert(elapsed.ns >= 100000000)
  })
  task.join(h)
  println("ok")
}
"#;
    let (stdout, stderr, code, _wall) = run_silt(src);
    assert_eq!(code, 0, "silt exit nonzero; stderr={stderr}");
    assert!(
        stdout.contains("ok"),
        "expected 'ok' in stdout; got {stdout:?}"
    );
}

#[test]
fn test_time_sleep_zero_duration_returns_immediately_no_yield() {
    // `time.sleep` with a zero duration should return Unit without
    // any yield or delay. This exercises the dur_ns <= 0 fast path
    // and preserves the pre-fix behavior.
    let src = r#"
import task
import time

fn main() {
  let h = task.spawn(fn() {
    time.sleep(time.ms(0))
    println("zero-ok")
  })
  task.join(h)
}
"#;
    let (stdout, stderr, code, wall) = run_silt(src);
    assert_eq!(code, 0, "silt exit nonzero; stderr={stderr}");
    assert!(
        stdout.contains("zero-ok"),
        "expected 'zero-ok' in stdout; got {stdout:?}"
    );
    // Very generous bound: the silt process itself takes time to spin
    // up. We just want to ensure the sleep(0) did NOT itself cause a
    // multi-second pause.
    assert!(
        wall < Duration::from_secs(5),
        "time.sleep(0) wall time {wall:?} unexpectedly high"
    );
}
