//! `SILT_IO_TIMEOUT` and the end of a program: a task that waits for
//! I/O is waited for, and the global timeout is what ends that wait
//! when nothing else does. The program ends then, by the rule, with
//! what the task did after its timeout.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run_with_io_timeout(
    name: &str,
    source: &str,
    io_timeout: &str,
) -> (Option<i32>, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "silt_stage7_io_timeout_{}_{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(&file, source).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .env("SILT_IO_TIMEOUT", io_timeout)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let limit = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);
            panic!("the program did not end although its I/O had a timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Nobody connects to the listener. Without the timeout the accept
/// would keep the program alive for ever; with it the task gets
/// `TcpTimeout`, goes on, and the program ends when it has.
#[test]
#[cfg(feature = "tcp")]
fn the_global_io_timeout_ends_a_wait_that_keeps_the_program_alive() {
    let source = r#"import task
import tcp

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  println("main returns")
  let _waits = task.spawn { ->
    match tcp.accept(listener) {
      Err(tcp.TcpTimeout) -> println("timed out")
      Ok(_) -> println("a connection")
      Err(e) -> println("another error: {e.message()}")
    }
  }
}
"#;
    let (code, out, err) = run_with_io_timeout("accept", source, "200ms");
    assert_eq!(out, "main returns\ntimed out\n", "stderr: {err}");
    assert_eq!(code, Some(0), "stderr: {err}");
}

/// A task that fails after its I/O timed out, after `main` returned,
/// still fails the run.
#[test]
#[cfg(feature = "tcp")]
fn a_failure_after_the_global_io_timeout_fails_the_run() {
    let source = r#"import task
import tcp

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let _waits = task.spawn { ->
    let _ = tcp.accept(listener)
    panic("after the timeout")
  }
}
"#;
    let (code, out, err) = run_with_io_timeout("failure", source, "200ms");
    assert_eq!(out, "");
    assert!(
        err.contains("failed and was never joined: panic: after the timeout"),
        "stderr: {err}"
    );
    assert_eq!(code, Some(1), "stderr: {err}");
}
