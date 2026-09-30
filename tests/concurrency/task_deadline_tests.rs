//! Integration tests for `task.deadline(dur, fn)`.
//!
//! Covers the invisible-timeout contract: I/O inside a scoped deadline
//! returns the standard `Err(String)` when the deadline elapses, without
//! any language-surface change.
//!
//! The deadline-at-entry, slack, scoping and nesting cases are golden
//! cases in `tests/golden/concurrency/deadline/task_deadline__*`. What
//! stays here needs process setup the golden harness cannot express: an
//! environment variable plus a held-open stdin (the `SILT_IO_TIMEOUT`
//! watchdog), and a Rust-side TCP listener.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
  let handle = task.spawn({ ->
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

#[cfg(feature = "tcp")]
#[test]
fn test_watchdog_fires_tcp_read_surfaces_tcp_timeout() {
    // Regression lock for the watchdog-on-parked-tcp-op path: when a
    // spawned task calls `tcp.read` on a stream that never receives
    // data, it parks in the io_pool. The scheduler's watchdog (armed
    // here via a scoped `task.spawn_until` deadline — `DeadlineSource::Task`)
    // must fire and complete the pending IoCompletion using the tcp
    // module's registered TimeoutErrFactory, surfacing `Err(TcpTimeout)`
    // to the silt-side match. Before per-completion factory plumbing,
    // the watchdog always wrote the generic `Err(IoUnknown(_))`, which
    // collided with tcp.*'s `Result(_, TcpError)` return shape and
    // forced callers into a wildcard arm.
    //
    // Complements `test_task_deadline_covers_tcp_connect_at_entry`
    // (ENTRY-guard path, deadline already past at builtin entry) and
    // `test_watchdog_env_var_fires_pending_io_surfaces_timeout_err`
    // (watchdog path for `io.read_line`). Without this test, the
    // watchdog path for tcp.* specifically is covered only by
    // scheduler unit tests, never end-to-end.
    //
    // Hermetic: a plain `TcpListener` bound to a loopback port holds
    // the connection open and never writes, so the silt-side
    // `tcp.read` parks until the watchdog fires.
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
    let addr = listener.local_addr().expect("local_addr").to_string();

    // Keep the accepted connection alive until the silt process exits.
    // The listener thread accepts one connection and parks — never
    // writes, never closes — so the client-side `tcp.read` in silt
    // has nothing to receive and will wait indefinitely without the
    // watchdog.
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    let server = std::thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("set_nonblocking on listener");
        let mut held: Option<std::net::TcpStream> = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !stop_t.load(Ordering::SeqCst) && Instant::now() < deadline {
            match listener.accept() {
                Ok((conn, _peer)) => {
                    // Hold the connection open; do not write, do not close.
                    held = Some(conn);
                    // Keep looping so we drain any extra connect attempts
                    // without crashing, but we won't write on any of them.
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        drop(held);
    });

    // Connect OUTSIDE the spawn_until budget so the deadline applies only
    // to the parked tcp.read — the actual regression target. With connect
    // inside the 50ms budget, a slow loopback connect on a loaded CI runner
    // (observed on the Windows "concurrency" partition) consumed the whole
    // budget, so the watchdog fired on connect and the program printed
    // "connect-err:tcp operation timed out" before reaching the read path.
    // connect's own deadline-at-entry behaviour is covered separately by
    // test_task_deadline_covers_tcp_connect_at_entry.
    let src = format!(
        r#"
import bytes
import tcp
import task
import time

fn main() {{
  match tcp.connect("{addr}") {{
    Ok(s) -> {{
      let outcome = task.spawn_until(time.ms(50), {{ ->
        match tcp.read(s, 1024) {{
          Ok(_) -> "unexpected-ok"
          Err(TcpTimeout) -> "tcp-timeout"
          Err(other) -> "other:" + other.message()
        }}
      }})
      println(task.join(outcome))
    }}
    Err(e) -> println("connect-err:" + e.message())
  }}
}}
"#
    );

    // Run silt with a bounded wall-clock wait so a regression in the
    // watchdog path surfaces as a loud test failure, not a CI hang.
    use std::sync::atomic::{AtomicU64, Ordering as Ord2};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ord2::SeqCst);
    let tmp = std::env::temp_dir().join(format!("silt_td_tcpwd_{}_{n}.silt", std::process::id()));
    std::fs::write(&tmp, &src).unwrap();

    let mut child = Command::new(silt_bin())
        .arg("run")
        .arg(&tmp)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn silt");

    let wait = Duration::from_secs(5);
    let start = Instant::now();
    let (stdout, stderr, code) = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut stdout);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut stderr);
                }
                break (stdout, stderr, status.code().unwrap_or(-1));
            }
            Ok(None) => {
                if start.elapsed() >= wait {
                    let _ = child.kill();
                    stop.store(true, Ordering::SeqCst);
                    let _ = server.join();
                    let _ = std::fs::remove_file(&tmp);
                    panic!(
                        "silt run did not exit within {wait:?} — watchdog-on-parked-tcp.read regressed"
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                stop.store(true, Ordering::SeqCst);
                let _ = server.join();
                let _ = std::fs::remove_file(&tmp);
                panic!("try_wait failed: {e}");
            }
        }
    };
    let _ = std::fs::remove_file(&tmp);
    stop.store(true, Ordering::SeqCst);
    let _ = server.join();

    assert_eq!(
        code, 0,
        "silt should exit 0 (timeout surfaces as a value, not a VM error); \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("tcp-timeout"),
        "watchdog-fired tcp.read must surface Err(TcpTimeout), not IoUnknown/IoInterrupted; \
         got stdout={stdout:?} stderr={stderr:?}"
    );
}
