//! `process.stop_signals` with real signals, sent with kill(2) to a
//! `silt run` child: the first SIGINT or SIGTERM is a message on the
//! channel, which is then closed, and the program ends as it chooses; a
//! wait on the channel is no deadlock; the next signal ends the process
//! as the system ends it; a program that never asks is ended by the
//! first, as before; a signal the process ignored stays ignored.
//!
//! Unix only (Linux and macOS). Windows is not tested here: a test cannot
//! send Ctrl-C or Ctrl-Break to one child process alone
//! (`GenerateConsoleCtrlEvent` reaches every process of a console's
//! process group, the test runner too), so no test runs the Windows
//! handler.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How long a step may take on a slow machine: each step waits for what
/// the program prints, no fixed sleep decides it.
const LIMIT: Duration = Duration::from_secs(20);

/// A `silt run` of a program, and the lines of its stdout as they come.
struct Program {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
    dir: PathBuf,
}

/// Start `source` as `main.silt` in a directory of its own; with
/// `ignore_sigint`, the process starts with SIGINT ignored, as a
/// command a shell script starts in the background does.
fn start(tag: &str, source: &str, ignore_sigint: bool) -> Program {
    let dir = std::env::temp_dir().join(format!("silt-stop-signals-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.silt"), source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_silt"));
    command
        .args(["run", "main.silt"])
        .current_dir(&dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if ignore_sigint {
        // SAFETY: signal(2) is async-signal-safe.
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGINT, libc::SIG_IGN);
                Ok(())
            });
        }
    }
    let mut child = command.spawn().expect("silt run starts");
    let stdout = child.stdout.take().unwrap();
    let (send, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    Program {
        child,
        lines,
        seen: Vec::new(),
        dir,
    }
}

impl Program {
    /// Wait until the program has printed the line `want`.
    fn wait_for(&mut self, want: &str) {
        let end = Instant::now() + LIMIT;
        while !self.seen.iter().any(|line| line == want) {
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => self.seen.push(line),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    let _ = self.child.kill();
                    panic!(
                        "the program never printed {want:?}; it printed {:?}",
                        self.seen
                    );
                }
            }
        }
    }

    fn signal(&self, signal: libc::c_int) {
        let pid = libc::pid_t::try_from(self.child.id()).unwrap();
        // SAFETY: kill(2) of the child this test started and has not reaped.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0, "kill");
    }

    /// Whether the program is still running `after` from now.
    fn runs_after(&mut self, after: Duration) -> bool {
        std::thread::sleep(after);
        self.child.try_wait().unwrap().is_none()
    }

    /// How the program ended, and every line it printed.
    fn finish(&mut self) -> (ExitStatus, Vec<String>) {
        let end = Instant::now() + LIMIT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= end {
                let _ = self.child.kill();
                panic!("the program did not end; it printed {:?}", self.seen);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        while let Ok(line) = self.lines.recv_timeout(LIMIT) {
            self.seen.push(line);
        }
        (status, self.seen.clone())
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Waits for the stop and saves its state: `main` waits on the stop
/// channel alone, which is no deadlock.
const SAVES_ON_STOP: &str = "import channel
import io
import process

fn main() {
  let stop = process.stop_signals()
  println(\"ready\")
  println(channel.receive(stop))
  println(channel.receive(stop))
  let _ = io.write_file(\"state.json\", \"[]\")
  println(\"drained\")
}
";

#[test]
fn the_first_stop_signal_is_a_message_and_the_program_ends_as_it_chooses() {
    for (signal, name, shown) in [
        (libc::SIGTERM, "term", "Some(Terminate)"),
        (libc::SIGINT, "int", "Some(Interrupt)"),
    ] {
        let mut program = start(name, SAVES_ON_STOP, false);
        program.wait_for("ready");
        program.signal(signal);
        let (status, lines) = program.finish();
        assert_eq!(status.code(), Some(0), "{name}: {lines:?}");
        assert_eq!(lines, ["ready", shown, "None", "drained"], "{name}");
        assert!(
            program.dir.join("state.json").exists(),
            "{name}: the state is saved"
        );
    }
}

#[test]
fn the_second_stop_signal_ends_the_process_as_the_system_does() {
    let source = "import channel
import process
import time

fn main() {
  let stop = process.stop_signals()
  println(\"ready\")
  let _ = channel.receive(stop)
  println(\"stopping\")
  time.sleep(time.ms(30000)) -- a shutdown that is stuck
  println(\"never\")
}
";
    let mut program = start("second", source, false);
    program.wait_for("ready");
    program.signal(libc::SIGTERM);
    program.wait_for("stopping");
    program.signal(libc::SIGINT);
    let (status, lines) = program.finish();
    assert_eq!(status.signal(), Some(libc::SIGINT), "{lines:?}");
    assert!(!lines.iter().any(|line| line == "never"), "{lines:?}");
}

#[test]
fn a_program_that_never_asks_is_ended_by_the_first_stop_signal() {
    let source = "import time

fn main() {
  println(\"ready\")
  time.sleep(time.ms(30000))
  println(\"never\")
}
";
    let mut program = start("never_asks", source, false);
    program.wait_for("ready");
    program.signal(libc::SIGTERM);
    let (status, lines) = program.finish();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{lines:?}");
}

#[test]
fn a_stop_signal_the_process_ignored_stays_ignored() {
    let mut program = start("ignored", SAVES_ON_STOP, true);
    program.wait_for("ready");
    program.signal(libc::SIGINT);
    assert!(
        program.runs_after(Duration::from_millis(500)),
        "SIGINT stays ignored"
    );
    program.signal(libc::SIGTERM);
    let (status, lines) = program.finish();
    assert_eq!(status.code(), Some(0), "{lines:?}");
    assert_eq!(lines, ["ready", "Some(Terminate)", "None", "drained"]);
}

/// The shape of a server that drains: the helpers of a
/// `task.background` wait for work that never comes, and its body for
/// the stop. Every task waits, and none of them is deadlocked: the stop
/// ends the body's wait, and the helpers are stopped with it.
#[test]
fn a_task_that_waits_for_the_stop_is_no_deadlock() {
    let source = "import channel
import process
import task

fn main() {
  let stop = process.stop_signals()
  let jobs = channel.new(0)
  let got = task.background([{ -> channel.each(jobs) { j -> println(j) } }]) { ->
    println(\"ready\")
    channel.receive(stop)
  }
  println(\"stopped by {got}\")
}
";
    let mut program = start("background", source, false);
    program.wait_for("ready");
    assert!(
        program.runs_after(Duration::from_millis(300)),
        "no deadlock is reported"
    );
    program.signal(libc::SIGTERM);
    let (status, lines) = program.finish();
    assert_eq!(status.code(), Some(0), "{lines:?}");
    assert_eq!(lines, ["ready", "stopped by Some(Terminate)"]);
}
