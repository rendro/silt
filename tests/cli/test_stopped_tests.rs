//! `silt test` stopped by a signal (Ctrl-C, SIGTERM): it names the test
//! it was in, with how long it had run, and the summary so far, and
//! exits with 1. A test's program that asks for the stop signals
//! (`process.stop_signals`) still gets them; the next test is listened
//! for again.
//!
//! Unix only (Linux and macOS), as `stop_signals_tests.rs`: a test
//! cannot send Ctrl-C to one child process alone on Windows.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How long a step may take on a slow machine: each step waits for what
/// the run prints, no fixed sleep decides it.
const LIMIT: Duration = Duration::from_secs(20);

/// A `silt test` of one test file, the lines of its stdout (what the
/// tests print) as they come, and its stderr (the runner's lines).
struct Run {
    child: Child,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
    dir: PathBuf,
}

fn lines_of(read: impl std::io::Read + Send + 'static) -> Receiver<String> {
    let (send, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(read).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    lines
}

/// Run `silt test h_test.silt` on `source` in a directory of its own.
fn start(tag: &str, source: &str) -> Run {
    start_with(tag, source, &["test", "h_test.silt"])
}

/// Run `silt <args>` with `source` as `h_test.silt` in a directory of
/// its own, as the leader of a process group of its own (as a shell
/// starts a command: a Ctrl-C at the terminal reaches the whole group).
fn start_with(tag: &str, source: &str, args: &[&str]) -> Run {
    let dir = std::env::temp_dir().join(format!("silt-test-stopped-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("h_test.silt"), source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_silt"));
    command
        .args(args)
        .current_dir(&dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().expect("silt starts");
    let stdout = lines_of(child.stdout.take().unwrap());
    let stderr = lines_of(child.stderr.take().unwrap());
    Run {
        child,
        stdout,
        stderr,
        dir,
    }
}

impl Run {
    /// Wait until a test has printed the line `want`.
    fn wait_for(&mut self, want: &str) {
        let end = Instant::now() + LIMIT;
        let mut seen = Vec::new();
        while !seen.iter().any(|line| line == want) {
            let left = end.saturating_duration_since(Instant::now());
            match self.stdout.recv_timeout(left) {
                Ok(line) => seen.push(line),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    let _ = self.child.kill();
                    panic!("the tests never printed {want:?}; they printed {seen:?}");
                }
            }
        }
    }

    fn signal(&self, signal: libc::c_int) {
        let pid = libc::pid_t::try_from(self.child.id()).unwrap();
        // SAFETY: kill(2) of the child this test started and has not reaped.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0, "kill");
    }

    /// Send `signal` to the run's whole process group, as a Ctrl-C at a
    /// terminal does.
    #[cfg(feature = "watch")]
    fn signal_group(&self, signal: libc::c_int) {
        let pid = libc::pid_t::try_from(self.child.id()).unwrap();
        // SAFETY: kill(2) of the process group the child leads.
        assert_eq!(unsafe { libc::kill(-pid, signal) }, 0, "kill");
    }

    /// The exit code of the run, and every line of its stderr.
    fn finish(&mut self) -> (Option<i32>, Vec<String>) {
        let end = Instant::now() + LIMIT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= end {
                let _ = self.child.kill();
                panic!("the run did not end");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut lines = Vec::new();
        while let Ok(line) = self.stderr.recv_timeout(LIMIT) {
            lines.push(line);
        }
        (status.code(), lines)
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Ok(pid) = libc::pid_t::try_from(self.child.id()) {
            // SAFETY: kill(2) of the process group the child leads
            // (`start_with`): nothing it started outlives the test.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The line that names the stopped `test`, without its time.
fn stopped_line<'a>(lines: &'a [String], test: &str) -> Option<&'a String> {
    let head = format!("  STOPPED h_test.silt::{test} (after ");
    lines
        .iter()
        .find(|line| line.starts_with(&head) && line.ends_with(" s)"))
}

const SPINS: &str = "import test

fn spin(n: Int) -> Int { spin(n) }

fn test_a() {
  test.assert(true)
}

fn test_b() {
  println(\"spinning\")
  let _ = spin(1)
}
";

#[test]
fn a_stopped_run_names_the_test_it_was_in_and_fails() {
    for (signal, name) in [(libc::SIGTERM, "term"), (libc::SIGINT, "int")] {
        let mut run = start(name, SPINS);
        run.wait_for("spinning");
        run.signal(signal);
        let (code, lines) = run.finish();
        assert_eq!(code, Some(1), "{name}: {lines:?}");
        assert!(
            stopped_line(&lines, "test_b").is_some(),
            "{name}: {lines:?}"
        );
        assert_eq!(lines[0], "  PASS h_test.silt::test_a", "{name}: {lines:?}");
        assert_eq!(
            lines.last().map(String::as_str),
            Some("2 tests: 1 passed, 0 failed, 0 skipped, 1 stopped"),
            "{name}: {lines:?}"
        );
    }
}

#[test]
fn a_run_stopped_in_the_top_level_code_names_the_file() {
    let source = "import test

fn spin(n: Int) -> Int { spin(n) }

let k = {
  println(\"spinning\")
  spin(1)
}

fn test_after() {
  test.assert(true)
}
";
    let mut run = start("top", source);
    run.wait_for("spinning");
    run.signal(libc::SIGTERM);
    let (code, lines) = run.finish();
    assert_eq!(code, Some(1), "{lines:?}");
    assert!(
        lines.iter().any(|line| line
            .starts_with("  STOPPED h_test.silt (in its top-level code, after ")
            && line.ends_with(" s)")),
        "{lines:?}"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("0 tests: 0 passed, 0 failed, 0 skipped"),
        "{lines:?}"
    );
}

#[test]
fn a_test_that_asks_for_the_stop_signals_gets_one_and_the_next_test_is_stopped() {
    let source = "import channel
import process
import test

fn spin(n: Int) -> Int { spin(n) }

fn test_waits_for_a_stop() {
  let stop = process.stop_signals()
  println(\"ready\")
  let got = channel.receive(stop)
  test.assert(got != None)
}

fn test_spins() {
  println(\"spinning\")
  let _ = spin(1)
}
";
    let mut run = start("asks", source);
    run.wait_for("ready");
    run.signal(libc::SIGINT);
    run.wait_for("spinning");
    run.signal(libc::SIGINT);
    let (code, lines) = run.finish();
    assert_eq!(code, Some(1), "{lines:?}");
    assert_eq!(
        lines[0], "  PASS h_test.silt::test_waits_for_a_stop",
        "{lines:?}"
    );
    assert!(stopped_line(&lines, "test_spins").is_some(), "{lines:?}");
    assert_eq!(
        lines.last().map(String::as_str),
        Some("2 tests: 1 passed, 0 failed, 0 skipped, 1 stopped"),
        "{lines:?}"
    );
}

/// Ctrl-C at the terminal under `silt test --watch`: the watcher and
/// the run it started both get the SIGINT; the watcher waits for the run
/// to end by it, so the run still names the test it stopped in. FAILS
/// on the base about half the time: the watcher's SIGTERM ended the run
/// before it had said so. Four rounds.
#[cfg(feature = "watch")]
#[test]
fn a_ctrl_c_under_watch_still_names_the_test() {
    for round in 0..4 {
        let tag = format!("watch{round}");
        let mut run = start_with(&tag, SPINS, &["test", "--watch", "h_test.silt"]);
        run.wait_for("spinning");
        run.signal_group(libc::SIGINT);
        let (_, lines) = run.finish();
        assert!(
            stopped_line(&lines, "test_b").is_some(),
            "round {round}: {lines:?}"
        );
    }
}
