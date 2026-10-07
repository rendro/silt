//! A task that waits for a line of input and is cancelled does not
//! keep the program alive: the read cannot be interrupted, but nobody
//! waits for it any more, so it is not pending for the program.
//!
//! A golden case cannot hold stdin open, hence a test of its own.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn a_cancelled_reader_of_stdin_does_not_keep_the_program() {
    let dir = std::env::temp_dir().join(format!("silt_stage7_stdin_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(
        &file,
        r#"import io
import task
import time

fn main() {
  let reader = task.spawn { -> io.read_line() }
  time.sleep(time.ms(20))
  task.cancel(reader)
  println("cancelled")
}
"#,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Held open, and nothing is written: the read never returns.
    let stdin = child.stdin.take().unwrap();
    let limit = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= limit {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(stdin);
    let _ = std::fs::remove_dir_all(&dir);
    let status = status.expect("the program ended while its stdin was open");
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert_eq!(out, "cancelled\n");
    assert!(status.success(), "{status:?}");
}
