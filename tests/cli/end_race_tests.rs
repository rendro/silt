//! Orders of the end of a program that once gave the wrong result, forced
//! by events in a build with `test-hooks` (no time is waited for); each
//! hook says on stderr that it ran.
//!
//! `SILT_TEST_END_RACE`: the program's thread drops the tasks that still
//! wait (a stream that one of them held to the end is let go, and the
//! stop of its stage is deferred), and a worker that has just ended the
//! last task runs that stop, after its slice, before the failures are
//! taken. The stop cancelled the stage and dismissed its failure: exit 0
//! and no report.
//!
//! `SILT_TEST_STOP_RACE`: a stop that was just deferred is taken by a
//! thread that counts for nothing, as a worker after its slice takes
//! one, and runs only once a look at the tasks has found it pending. At
//! `task-end`, the task that let go of the stream stopped counting with
//! the stop not yet run, and the program's thread was told "deadlock on
//! main thread" while the stop was to wake it. At `main-returns`, the
//! end of the program kept the failure of a stream that `main` had left.

#![cfg(feature = "test-hooks")]

use std::process::{Command, Stdio};

/// `silt run` of `source` with the variable `hook` set to `value`:
/// (status, stdout, stderr).
fn run_with(name: &str, source: &str, hook: &str, value: &str) -> (Option<i32>, String, String) {
    let path =
        std::env::temp_dir().join(format!("silt_end_race_{name}_{}.silt", std::process::id()));
    std::fs::write(&path, source).expect("write temp file");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&path)
        .env("NO_COLOR", "1")
        .env_remove("SILT_TEST_END_RACE")
        .env_remove("SILT_TEST_STOP_RACE")
        .env(hook, value)
        .stdin(Stdio::null())
        .output()
        .expect("spawn silt");
    let _ = std::fs::remove_file(&path);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

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
    let (code, stdout, stderr) = run_with("held", HELD_TO_THE_END, "SILT_TEST_END_RACE", "1");
    assert_eq!(stdout, "done\n");
    assert!(
        stderr.contains("silt test hook: the deferred stops ran after the end"),
        "the hook did not run; stderr: {stderr}"
    );
    assert!(
        stderr.contains("a `stream.buffered` stage failed and its output was never read"),
        "no report; stderr: {stderr}"
    );
    assert_eq!(code, Some(1), "stderr: {stderr}");
}

/// A task lets go of the stream `s`, whose stop closes `ends`, on which
/// main waits (fix/rv-zb/probes/g2_stop_at_start_many.silt, once). The
/// task asks `s` for a value first, so its stage waits on its input, not
/// to be asked: nothing but its stop ends that wait (a stage waiting to be
/// asked ends by itself when its output is gone), and without the stop
/// main is deadlocked.
const LEFT_BY_A_TASK: &str = r#"import channel
import stream
import task

fn main() {
  let never: Channel(Int) = channel.new(0)
  let ends = stream.map(never) { x -> x }
  let _t = task.spawn { ->
    let s = stream.map(ends) { x -> x }
    let _ = channel.try_receive(s)
    ()
  }
  let _ = channel.receive(ends)
  println("stopped")
}
"#;

#[test]
fn a_stop_taken_by_a_thread_that_counts_for_nothing_is_waited_for() {
    let (code, stdout, stderr) = run_with(
        "task_end",
        LEFT_BY_A_TASK,
        "SILT_TEST_STOP_RACE",
        "task-end",
    );
    assert!(
        stderr.contains("silt test hook: a thread that counts for nothing ran the stops"),
        "the hook did not run; stderr: {stderr}"
    );
    assert!(!stderr.contains("deadlock"), "stderr: {stderr}");
    assert_eq!(stdout, "stopped\n", "stderr: {stderr}");
    assert_eq!(code, Some(0), "stderr: {stderr}");
}

/// `main` leaves a stream a stage of which fails (card STR-1's program):
/// its failure is not reported.
const LEFT_BY_MAIN: &str = r#"import stream

fn main() {
  let a = stream.from_list([1, 2, 3]) |> stream.map { x -> 10 / (x - 2) }
  let _b = stream.buffered(a, 4)
  println("done")
}
"#;

#[test]
fn a_stop_of_a_left_stream_taken_by_such_a_thread_still_dismisses_its_failure() {
    let (code, stdout, stderr) = run_with(
        "main_returns",
        LEFT_BY_MAIN,
        "SILT_TEST_STOP_RACE",
        "main-returns",
    );
    assert!(
        stderr.contains("silt test hook: a thread that counts for nothing ran the stops"),
        "the hook did not run; stderr: {stderr}"
    );
    assert_eq!(stdout, "done\n");
    assert!(!stderr.contains("error"), "stderr: {stderr}");
    assert_eq!(code, Some(0), "stderr: {stderr}");
}
