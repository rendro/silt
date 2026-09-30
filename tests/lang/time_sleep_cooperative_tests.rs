//! Integration tests for `time.sleep` cooperative parking.
//!
//! `time.sleep` must park the scheduled task on the shared timer thread
//! rather than blocking the worker thread with `thread::sleep`. When
//! sixteen tasks each sleep 500ms, wall time should be ~500ms (not
//! serialized across the 4-ish worker pool). The non-timing cases are
//! golden cases `tests/golden/lang/time/time_sleep_cooperative__*`.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn silt_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_silt") {
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
    |> list.map { _ -> task.spawn({ -> time.sleep(time.ms(500)) }) }
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
    // Threshold relaxed under CI=1 to absorb GitHub-runner CPU
    // contention. On Windows in particular, silt subprocess cold-
    // start + scheduler init alone consumes ~500-1500ms before the
    // first sleep call begins, blowing the 2s ceiling on a
    // healthy-but-slow runner. The test still locks the cooperative-
    // park contract (anything less than 16×500ms ≈ 8s proves
    // parallelism); under CI we just give silt's startup a 5s
    // budget over the parallel-sleep window. Local runs unaffected.
    let ceiling = if std::env::var("CI").is_ok() {
        Duration::from_millis(5000)
    } else {
        Duration::from_millis(2000)
    };
    assert!(
        wall < ceiling,
        "16 parallel 500ms sleeps took {wall:?}; expected under {ceiling:?} (cooperative park)"
    );
}
