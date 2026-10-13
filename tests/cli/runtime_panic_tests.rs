//! What the `silt` binary does when a thread of its runtime panics: it
//! ends, with Rust's own lines for the panic (the thread, the place,
//! the message), one line that says it is a bug in silt, and exit
//! status 101. Before, a panic on a worker, the timer's thread or a
//! thread of the I/O pool left the process hanging: the tasks that
//! waited for what the lost thread was doing, and `main` with them,
//! waited for ever.
//!
//! No silt program can make the runtime panic. A build with the
//! `test-hooks` feature reads `SILT_TEST_PANIC` in `silt run` and makes
//! the runtime panic once at the place it names; every other build
//! ignores the variable, which the last test here shows for a build
//! without the feature.
//!
//! No test asserts a time. A process that does not end is killed after
//! a minute so that the test fails instead of hanging the run.

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What `silt run` of `source` wrote and how it ended, with
/// `SILT_TEST_PANIC` set to `place` if there is one.
fn run(name: &str, source: &str, place: Option<&str>) -> (Option<i32>, String, String) {
    let dir =
        std::env::temp_dir().join(format!("silt_runtime_panic_{}_{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("a directory for the program");
    fs::write(dir.join("main.silt"), source).expect("the program is written");
    let mut command = Command::new(env!("CARGO_BIN_EXE_silt"));
    command
        .args(["run", "main.silt"])
        .current_dir(&dir)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .env_remove("RUST_BACKTRACE")
        .env_remove("SILT_TEST_PANIC")
        .stdin(Stdio::null())
        .stdout(fs::File::create(dir.join("out")).expect("a file for stdout"))
        .stderr(fs::File::create(dir.join("err")).expect("a file for stderr"));
    if let Some(place) = place {
        command.env("SILT_TEST_PANIC", place);
    }
    let mut child = command.spawn().expect("silt starts");
    let started = Instant::now();
    let status = loop {
        match child.try_wait().expect("the process is asked") {
            Some(status) => break status,
            None if started.elapsed() >= Duration::from_secs(60) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("silt run did not end: the program hangs ({name})");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let read = |file: &str| fs::read_to_string(dir.join(file)).expect("the output is read");
    let ended = (status.code(), read("out"), read("err"));
    let _ = fs::remove_dir_all(&dir);
    ended
}

/// `<handle:0>` waits to be woken and would then send on a channel
/// that nobody else sends on; a task joins it, a task receives on that
/// channel, and `main` joins both. It is woken when all of them wait
/// (the sleep makes that likely; nothing depends on it).
const LOST_WHILE_ALL_WAIT: &str = r#"import channel
import task
import time

fn main() {
  let gate = channel.new(0)
  let only = channel.new(0)
  let lost = task.spawn { ->
    let _ = channel.receive(gate)
    channel.send(only, 1)
  }
  let joiner = task.spawn { -> task.join(lost) }
  let receiver = task.spawn { -> channel.receive(only) }
  let waker = task.spawn { ->
    time.sleep(time.ms(20))
    channel.send(gate, 1)
  }
  println("main waits")
  task.join(joiner)
  let _ = task.join(receiver)
  task.join(waker)
  println("main went on")
}
"#;

/// The process ended as silt ends after a panic of `thread` made at
/// `place`: status 101; what `main` printed before; Rust's lines for
/// the panic, and then, as the last line, the one that calls it a bug.
#[cfg(feature = "test-hooks")]
fn assert_internal_error(ended: (Option<i32>, String, String), thread: &str, place: &str) {
    let (status, stdout, stderr) = ended;
    assert_eq!(status, Some(101), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "main waits\n", "stderr: {stderr}");
    let message = format!("a panic made for a test: {place}");
    let rust = format!("thread '{thread}' ");
    let lines: Vec<&str> = stderr.lines().collect();
    let at = lines.iter().position(|line| line.starts_with(&rust));
    let at = at.unwrap_or_else(|| panic!("no line of Rust's for the panic in: {stderr}"));
    assert!(lines[at].contains(" panicked at src"), "{stderr}");
    assert_eq!(lines[at + 1], message, "{stderr}");
    assert_eq!(
        lines.last().copied(),
        Some(
            format!(
                "silt: internal error: {message}. This is a bug in silt, not in your program: \
                 please report it."
            )
            .as_str()
        ),
        "{stderr}"
    );
    assert!(
        !stderr.contains("error["),
        "no error of the program: {stderr}"
    );
}

/// A worker panics as it is about to run a task that a joiner, a
/// receiver and `main` all wait on.
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_a_worker_ends_the_program() {
    let ended = run("worker", LOST_WHILE_ALL_WAIT, Some("task:0"));
    assert_internal_error(ended, "silt-worker", "Task(0)");
}

/// The timer's thread panics when the first deadline is due, while a
/// task sleeps and `main` joins it.
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_the_timer_thread_ends_the_program() {
    let source = r#"import task
import time

fn main() {
  let sleeper = task.spawn { -> time.sleep(time.ms(5)) }
  println("main waits")
  task.join(sleeper)
  println("main went on")
}
"#;
    let ended = run("timer", source, Some("timer"));
    assert_internal_error(ended, "silt-timer", "Timer");
}

/// A thread of the I/O pool panics when its operation has returned,
/// before it hands the value to the task that waits for it, which
/// `main` joins.
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_an_io_thread_ends_the_program() {
    let source = r#"import io
import task

fn main() {
  let reader = task.spawn { -> io.read_file("main.silt") }
  println("main waits")
  let _ = task.join(reader)
  println("main went on")
}
"#;
    let ended = run("io", source, Some("io"));
    assert_internal_error(ended, "silt-io", "Io");
}

/// The thread that runs `main` panics in the VM's code while a task
/// waits on a channel: the process ended before this step too, and now
/// says what it is.
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_the_program_thread_ends_the_program() {
    let source = r#"import channel
import task
import time

fn main() {
  let never = channel.new(0)
  let waits = task.spawn { -> channel.receive(never) }
  println("main waits")
  let _ = channel.receive_timeout(never, time.ms(1))
  println("main went on")
}
"#;
    let ended = run("program", source, Some("program"));
    assert_internal_error(ended, "silt-main", "Program");
}

/// Without the variable the program of the first test runs to its end.
#[test]
fn the_program_runs_to_its_end_when_nothing_panics() {
    let (status, stdout, stderr) = run("plain", LOST_WHILE_ALL_WAIT, None);
    assert_eq!(stderr, "");
    assert_eq!(stdout, "main waits\nmain went on\n");
    assert_eq!(status, Some(0));
}

/// A build without `test-hooks` does not read the variable: the
/// program runs to its end with it set.
#[test]
#[cfg(not(feature = "test-hooks"))]
fn a_build_without_the_test_hooks_ignores_the_variable() {
    let (status, stdout, stderr) = run("ignored", LOST_WHILE_ALL_WAIT, Some("task:0"));
    assert_eq!(stderr, "");
    assert_eq!(stdout, "main waits\nmain went on\n");
    assert_eq!(status, Some(0));
}
