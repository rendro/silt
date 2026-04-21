//! Integration tests for `task.deadline(dur, fn)`.
//!
//! Covers the invisible-timeout contract: I/O inside a scoped deadline
//! returns the standard `Err(String)` when the deadline elapses, without
//! any language-surface change.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Render a path for embedding inside silt source. `path.display()`
/// uses native separators, so on Windows `\U` et al. become unknown
/// lexer escape sequences. Forward slashes are accepted by the
/// Windows filesystem APIs so the runtime still resolves the file.
fn path_for_silt(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "/")
}

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
    let tmp = std::env::temp_dir().join(format!("silt_td_{}_{n}.silt", std::process::id()));
    std::fs::write(&tmp, src).unwrap();
    let output = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .output()
        .unwrap();
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
        path_for_silt(&path)
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
        path_for_silt(&path)
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
fn test_deadline_exceeded_pending_io_does_not_leak_to_next_call() {
    // Regression lock for B1: when task.deadline elapses before the
    // inner I/O completes, the deadline-exceeded early-exit path must
    // clear pending_io so that a subsequent I/O call (outside the
    // scope) does not reuse the stale completion.
    //
    // Note: this test covers the *task.deadline early-exit* path, not
    // the SILT_IO_TIMEOUT watchdog-thread path — see
    // `test_watchdog_env_var_fires_pending_io_surfaces_timeout_err`
    // for the watchdog-writes-Err-to-completion variant. The helper
    // `run_silt()` deliberately does NOT set SILT_IO_TIMEOUT, so the
    // watchdog registry is None and the watchdog thread never runs
    // here; only `task.deadline` imposes a scope.
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
        path_for_silt(&fixture),
        path_for_silt(&fixture)
    );
    let (stdout, _stderr, code) = run_silt(&src);
    let _ = std::fs::remove_file(&fixture);
    assert_eq!(code, 0);
    // The second I/O must succeed (or get a clean fs error) — it must
    // NOT return the stale watchdog-fired Err.
    assert!(
        stdout.contains("second=") && !stdout.contains("second=err:leak"),
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
      Err(e) -> e.message()
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
      Err(e) -> e.message()
    }}
  }})
  println(task.join(handle))
}}
"#,
        path_for_silt(&path)
    );
    let (stdout, _stderr, code) = run_silt(&src);
    let _ = std::fs::remove_file(&path);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "done");
}

#[test]
fn test_task_deadline_covers_http_get_at_entry() {
    // task.deadline with an already-past deadline must short-circuit
    // http.get without submitting the request to the I/O pool. Same
    // contract as io.read_file — the shared vm.io_entry_guard is what
    // makes this work uniformly across every I/O builtin.
    let (stdout, _stderr, code) = run_silt(
        r#"
import http
import task
import time

fn main() {
  let outcome = task.deadline(time.ms(0), fn() {
    http.get("http://127.0.0.1:1/does-not-exist")
  })
  match outcome {
    Ok(resp) -> println("unexpected ok")
    Err(msg) -> println(msg)
  }
}
"#,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("I/O timeout (task.deadline exceeded)"),
        "http.get must respect task.deadline at entry; got stdout={stdout:?}"
    );
}

/// Run `silt run <tmp>` with `SILT_IO_TIMEOUT=<val>` set and stdin
/// held open (piped, never written) so that a spawned task calling
/// `io.read_line()` parks inside the I/O pool long enough for the
/// real watchdog thread to fire. Bounded wall-clock wait guards
/// against a hang if the watchdog path regresses.
///
/// Returns (stdout, stderr, exit_code). On timeout the child is
/// killed and the function panics with a clear diagnostic.
fn run_silt_with_io_timeout_stdin_piped(
    src: &str,
    io_timeout: &str,
    wait: Duration,
) -> (String, String, i32) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("silt_td_wd_{}_{n}.silt", std::process::id()));
    std::fs::write(&tmp, src).unwrap();

    let mut child = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .env("SILT_IO_TIMEOUT", io_timeout)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn silt");

    // Hold stdin open (never write, never drop it before exit) so
    // io.read_line in the spawned task parks in the kernel until
    // the watchdog fires.
    let stdin_handle = child.stdin.take();

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = std::fs::remove_file(&tmp);
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut stdout);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut stderr);
                }
                drop(stdin_handle);
                return (stdout, stderr, status.code().unwrap_or(-1));
            }
            Ok(None) => {
                if start.elapsed() >= wait {
                    let _ = child.kill();
                    let _ = std::fs::remove_file(&tmp);
                    panic!(
                        "silt run did not exit within {wait:?} — watchdog path likely regressed"
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                panic!("try_wait failed: {e}");
            }
        }
    }
}

#[test]
fn test_watchdog_env_var_fires_pending_io_surfaces_timeout_err() {
    // Regression lock for the SILT_IO_TIMEOUT watchdog-thread path:
    // when an I/O submit parks in the I/O pool and does NOT complete
    // before the global timeout, the watchdog thread must write the
    // canonical Err ("I/O timeout (SILT_IO_TIMEOUT exceeded)") into
    // the completion slot, and the parked task must resume with that
    // Err instead of hanging forever.
    //
    // Setup: stdin is piped open (never closed, never written) so
    // io.read_line inside the spawned task parks in the I/O pool's
    // blocking read until the watchdog fires.
    //
    // This complements `test_deadline_exceeded_pending_io_does_not_leak_to_next_call`
    // (which exercises the *task.deadline early-exit* path — no
    // watchdog thread running). Here the real watchdog thread runs
    // because SILT_IO_TIMEOUT is set via Command::env(); without this
    // test, the watchdog-writes-Err-to-completion path is covered
    // only by scheduler unit tests, never end-to-end.
    //
    // Round-23 (commit 590c2d8) resolved the scheduler deadlock this
    // test was gated against, so it now runs unconditionally as a
    // regression lock for the watchdog-writes-Err-to-completion path.
    let src = r#"
import io
import task

fn main() {
  let handle = task.spawn(fn() {
    match io.read_line() {
      Ok(_) -> "unexpected_ok"
      Err(e) -> e.message()
    }
  })
  println(task.join(handle))
}
"#;
    let (stdout, stderr, code) =
        run_silt_with_io_timeout_stdin_piped(src, "50ms", Duration::from_secs(15));
    assert_eq!(
        code, 0,
        "silt should exit 0 (watchdog Err surfaces as a value, not a VM error); \
         stdout={stdout:?} stderr={stderr:?}"
    );
    // Canonical message from DeadlineSource::Global in src/scheduler.rs:
    // "I/O timeout (SILT_IO_TIMEOUT exceeded)". Assert on the two
    // load-bearing substrings so a non-substantive wording tweak
    // doesn't flap this regression lock.
    assert!(
        stdout.contains("I/O timeout") && stdout.contains("SILT_IO_TIMEOUT"),
        "expected watchdog Err message with 'I/O timeout' and 'SILT_IO_TIMEOUT'; \
         got stdout={stdout:?} stderr={stderr:?}"
    );
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
