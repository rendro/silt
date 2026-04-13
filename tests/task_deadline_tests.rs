//! Integration tests for `task.deadline(dur, fn)`.
//!
//! Covers the invisible-timeout contract: I/O inside a scoped deadline
//! returns the standard `Err(String)` when the deadline elapses, without
//! any language-surface change.

use std::path::PathBuf;
use std::process::Command;

fn silt_bin() -> PathBuf {
    let target = std::env::var("CARGO_BIN_EXE_silt").ok();
    if let Some(p) = target {
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

fn run_silt(src: &str) -> (String, String, i32) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!(
        "silt_td_{}_{n}.silt",
        std::process::id()
    ));
    std::fs::write(&tmp, src).unwrap();
    let output = Command::new(silt_bin()).arg("run").arg(&tmp).output().unwrap();
    let _ = std::fs::remove_file(&tmp);
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn test_task_deadline_zero_ms_returns_immediate_timeout_err() {
    // A 0ms deadline means the deadline is already in the past at the
    // moment io.read_file runs. The builtin's entry check returns the
    // standard Err variant without submitting to the I/O pool.
    let (stdout, _stderr, code) = run_silt(
        r#"
import io
import task
import time

fn main() {
  let outcome = task.deadline(time.ms(0), fn() {
    io.read_file("/tmp/silt_td_unused.txt")
  })
  match outcome {
    Ok(s) -> println(s)
    Err(msg) -> println(msg)
  }
}
"#,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("I/O timeout (task.deadline exceeded)"),
        "expected deadline-exceeded message; got stdout={stdout:?}"
    );
}

#[test]
fn test_task_deadline_with_slack_completes_normally() {
    // A deadline with generous slack must NOT fire; fast I/O completes
    // with the real Ok result.
    let path = std::env::temp_dir().join("silt_td_fast.txt");
    std::fs::write(&path, "ready").unwrap();
    let src = format!(
        r#"
import io
import task
import time

fn main() {{
  let outcome = task.deadline(time.seconds(60), fn() {{
    io.read_file("{}")
  }})
  match outcome {{
    Ok(content) -> println(content)
    Err(msg) -> println(msg)
  }}
}}
"#,
        path.display()
    );
    let (stdout, _stderr, code) = run_silt(&src);
    let _ = std::fs::remove_file(&path);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "ready");
}

#[test]
fn test_task_deadline_does_not_leak_to_later_io() {
    // After task.deadline's scope ends, the deadline must be cleared so
    // subsequent I/O outside the scope runs with no deadline.
    let path = std::env::temp_dir().join("silt_td_leak.txt");
    std::fs::write(&path, "after").unwrap();
    let src = format!(
        r#"
import io
import task
import time

fn main() {{
  let _inside = task.deadline(time.ms(0), fn() {{
    io.read_file("/nonexistent_path")
  }})
  match io.read_file("{}") {{
    Ok(content) -> println(content)
    Err(msg) -> println(msg)
  }}
}}
"#,
        path.display()
    );
    let (stdout, _stderr, code) = run_silt(&src);
    let _ = std::fs::remove_file(&path);
    assert_eq!(code, 0);
    assert_eq!(
        stdout.trim(),
        "after",
        "I/O after task.deadline scope must NOT inherit the elapsed deadline"
    );
}

#[test]
fn test_watchdog_fired_pending_io_does_not_leak_to_next_call() {
    // Regression lock for B1: when an I/O submit yields, the watchdog
    // fires Err into pending_io, the task resumes, and the builtin
    // short-circuits via deadline_exceeded() — the reconcile path must
    // consume pending_io first, otherwise a subsequent I/O call outside
    // the deadline scope would read the stale watchdog Err.
    //
    // Setup: SILT_IO_TIMEOUT=50ms forces the watchdog to fire quickly.
    // task.spawn_until(50ms) does an I/O that yields (real file,
    // watchdog wins the race). After deadline-Err surfaces, a second
    // I/O in the SAME spawned task (post-deadline) must see its own
    // fresh result (Err on missing file), not the stale deadline Err.
    let fixture = std::env::temp_dir().join("silt_td_b1_ok.txt");
    std::fs::write(&fixture, "ok").unwrap();
    let src = format!(
        r#"
import io
import task
import time

fn main() {{
  -- First I/O inside deadline that times out mid-park.
  let first = task.deadline(time.ms(1), fn() {{
    io.read_file("{}")
  }})
  -- Second I/O AFTER the deadline scope — must NOT reuse
  -- the stale pending_io from the first call.
  let second = io.read_file("{}")
  match first {{
    Ok(s) -> println("first=ok:" )
    Err(m) -> println("first=err")
  }}
  match second {{
    Ok(s) -> println("second=" )
    Err(m) -> println("second=err:leak")
  }}
}}
"#,
        fixture.display(),
        fixture.display()
    );
    let (stdout, _stderr, code) = run_silt(&src);
    let _ = std::fs::remove_file(&fixture);
    assert_eq!(code, 0);
    // The second I/O must succeed (or get a clean fs error) — it must
    // NOT return the stale watchdog-fired Err.
    assert!(
        stdout.contains("second=")
            && !stdout.contains("second=err:leak"),
        "second I/O must not reuse stale pending_io; got: {stdout}"
    );
}

#[test]
fn test_task_spawn_until_zero_ms_child_io_times_out() {
    // task.spawn_until installs a deadline on the child task. With 0ms,
    // every I/O in the child returns Err immediately at entry.
    let (stdout, _stderr, code) = run_silt(
        r#"
import io
import task
import time

fn main() {
  let handle = task.spawn_until(time.ms(0), fn() {
    match io.read_file("/tmp/silt_spawn_until_noent") {
      Ok(s) -> s
      Err(msg) -> msg
    }
  })
  println(task.join(handle))
}
"#,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("I/O timeout (task.deadline exceeded)"),
        "child should timeout at I/O entry; got stdout={stdout:?}"
    );
}

#[test]
fn test_task_spawn_until_slack_completes_normally() {
    // Generous deadline — child completes with real result.
    let path = std::env::temp_dir().join("silt_td_spawn_slack.txt");
    std::fs::write(&path, "done").unwrap();
    let src = format!(
        r#"
import io
import task
import time

fn main() {{
  let handle = task.spawn_until(time.seconds(60), fn() {{
    match io.read_file("{}") {{
      Ok(s) -> s
      Err(msg) -> msg
    }}
  }})
  println(task.join(handle))
}}
"#,
        path.display()
    );
    let (stdout, _stderr, code) = run_silt(&src);
    let _ = std::fs::remove_file(&path);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "done");
}

#[test]
fn test_task_deadline_nested_synchronous_tightens() {
    // Outer deadline 60s; inner deadline 0ms. Inner's tighter deadline
    // wins inside the inner scope.
    let (stdout, _stderr, code) = run_silt(
        r#"
import io
import task
import time

fn main() {
  let outcome = task.deadline(time.seconds(60), fn() {
    task.deadline(time.ms(0), fn() {
      io.read_file("/nonexistent_silt_inner")
    })
  })
  match outcome {
    Ok(s) -> println(s)
    Err(msg) -> println(msg)
  }
}
"#,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("I/O timeout (task.deadline exceeded)"),
        "inner tighter deadline should fire; got stdout={stdout:?}"
    );
}
