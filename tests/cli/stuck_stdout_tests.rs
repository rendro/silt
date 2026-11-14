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
