//! The end of a program in the order that once lost a failure: the
//! program's thread drops the tasks that still wait (a stream that one
//! of them held to the end is let go, and the stop of its stage is
//! deferred), and a worker that has just ended the last task runs that
//! stop, after its slice, before the program's thread takes the
//! failures. The stop cancelled the stage and dismissed its failure:
//! the run ended with exit 0 and no report, now and then. A build with
//! `test-hooks` runs the deferred stops at that moment when
//! `SILT_TEST_END_RACE` is set (no time is waited for), and says on
//! stderr that it did.

use std::process::{Command, Stdio};

/// A task holds the stream `b` to the end, and a stage of it fails
/// (tests/golden/concurrency/streams/
/// wave7_s1__a_failure_the_next_stage_raised_is_reported_once).
const HELD_TO_THE_END: &str = r#"import channel
import stream
import task

fn main() {
  let src = stream.from_list([1, 2, 3])
  let a = stream.map(src) { x -> 10 / (x - 2) }
  let b = stream.buffered(a, 4)
  let never: Channel(Int) = channel.new(0)
  let _holds = task.spawn { ->
    let _ = channel.receive(never)
    b
  }
  println("done")
}
"#;

#[test]
fn a_stop_that_runs_after_the_end_dismisses_no_failure() {
    let path = std::env::temp_dir().join(format!("silt_end_race_{}.silt", std::process::id()));
    std::fs::write(&path, HELD_TO_THE_END).expect("write temp file");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&path)
        .env("NO_COLOR", "1")
        .env("SILT_TEST_END_RACE", "1")
        .stdin(Stdio::null())
        .output()
        .expect("spawn silt");
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stdout, "done\n");
    assert!(
        stderr.contains("silt test hook: the deferred stops ran after the end"),
        "the hook did not run; stderr: {stderr}"
    );
    assert!(
        stderr.contains("a `stream.buffered` stage failed and its output was never read"),
        "no report; stderr: {stderr}"
    );
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
}
