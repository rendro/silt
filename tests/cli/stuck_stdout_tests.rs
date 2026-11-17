//! A program that ends by a failure while its stdout is stuck (a pipe
//! nobody reads) says so: the error is written, and the status is the
//! failure's, after the output's writer was waited for only while it got
//! rid of some of it. (The end of the run waited for the reader without a
//! bound: the failure was not shown while the reader took nothing, and
//! when it went away the process ended with 141 and no message.)

#![cfg(unix)]

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Run `source` with stdout into a pipe that is held and never read:
/// (status, stderr). The program must end within 30 s.
fn run_with_a_stuck_stdout(name: &str, source: &str) -> (Option<i32>, String) {
    let dir = std::env::temp_dir().join(format!("silt_stuck_stdout_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(&file, source).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Held, not read, until the program has ended.
    let stuck = child.stdout.take();
    let limit = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > limit {
            let _ = child.kill();
            panic!("the failed program did not end while its stdout was stuck");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stuck);
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (status.code(), err)
}

/// A task prints more than a pipe holds; main panics.
#[test]
fn a_panic_is_reported_while_stdout_is_stuck() {
    let (code, err) = run_with_a_stuck_stdout(
        "panic",
        r#"import list
import string
import task
import time

fn main() {
  let line = string.repeat("x", 1000)
  let _printer = task.spawn { -> list.each(1..2000) { _ -> println(line) } }
  time.sleep(time.ms(300))
  panic("boom")
}
"#,
    );
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("error[runtime]: panic: boom"), "{err}");
}

/// The threads of the process `pid`, by its task directory (Linux).
#[cfg(target_os = "linux")]
fn threads_of(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .map(|tasks| tasks.count())
        .unwrap_or(0)
}

/// Forty tasks each printing a text longer than a print makes whole, to a
/// stdout nobody reads yet: one of them goes, the others wait their turn,
/// and a waiting long print holds no thread. FAIL on 0.43.1: each long
/// print started the thread that makes its text at once (forty threads
/// beside the runtime's), which then waited with the pieces it had made.
#[cfg(target_os = "linux")]
#[test]
fn long_prints_that_wait_their_turn_hold_no_thread() {
    let dir = std::env::temp_dir().join(format!("silt_long_prints_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(
        &file,
        "import list\nimport string\nimport task\n\nfn main() {\n  let text = string.repeat(\"x\", 2000000)\n  \
         let tasks = 1..40 |> list.map { _ -> task.spawn { -> println(text) } }\n  \
         tasks |> list.each { t -> task.join(t) }\n}\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Held, not read, while the prints start and wait; the most threads
    // the process had meanwhile.
    let mut stuck = child.stdout.take().unwrap();
    let mut most = 0;
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        most = most.max(threads_of(child.id()));
        std::thread::sleep(Duration::from_millis(50));
    }
    // Then read all of it: the program ends.
    let mut out = Vec::new();
    stuck.read_to_end(&mut out).unwrap();
    let status = child.wait().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(status.success(), "{status:?}");
    assert_eq!(out.len(), 40 * 2_000_001);
    assert!(
        most < 30,
        "the process had {most} threads while forty long prints waited: each holds one"
    );
}
