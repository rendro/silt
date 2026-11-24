//! Channels and tasks: lost wake-ups, stopping a task, worker stacks,
//! timers in the deadlock detection.
//!
//! Four defects are locked here.
//!
//!   * H1. A buffered channel woke a parked sender only when a receive
//!     found the buffer full. Several receives in a row woke one
//!     sender, and the other senders stayed parked for good: the
//!     worker-pool shape hung. The same class: a `channel.select` that
//!     was woken for one arm and completed another one used up the
//!     wake-up, and a send that raced a close could be accepted after
//!     the receiver had seen `Closed`.
//!   * H2. Stopping a task could hang the runtime: the task handle ran
//!     the cancel cleanup while it held the lock on the cleanup, and
//!     the cleanup took the lock of the parked task, which the workers
//!     and the wakers take first.
//!   * H3. Scheduler workers had the default stack of 2 MiB, so
//!     recursion through a callback inside a task overflowed the
//!     native stack at a small depth and aborted the process.
//!   * H4. A task that waits for a timer was counted as stuck, so a
//!     main thread that waited for that task's send was told
//!     "deadlock".
//!
//! Most scenarios are golden cases:
//! `tests/golden/concurrency/*/wave1_channels_tasks__*`. What stays here
//! needs more than the golden format expresses: output whose order
//! depends on scheduling (checked line by line) and a run under
//! `RLIMIT_AS`.
//!
//! Every test runs the built `silt` binary on a program in a fresh
//! temporary directory and asserts on exit status, stdout and stderr.
//! Every run has a kill timeout, so a hang fails the test instead of
//! hanging the suite. A scenario that can hang runs ten times. The assertions are on results, never on elapsed time.
//!
//! Tests named `guard_*` pin behaviour that must not change; they pass
//! with and without the fixes. All other tests fail without them. Where
//! the defect is a race, "fail" means that a run fails with the
//! frequency given at the test, and the test runs the scenario often
//! enough to meet a failing run; the rarest one is
//! `h1_value_accepted_before_close_is_delivered` (now a golden case).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Upper bound for one run of the binary. A run that exceeds it is
/// killed and counts as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

/// How often a scenario runs whose defect is a hang.
const HANG_REPEATS: usize = 10;

/// Every main-thread deadlock diagnostic starts with this.
const DEADLOCK: &str = "deadlock on main thread";

#[derive(Debug)]
struct Outcome {
    /// Exit status; `None` if the process was ended by a signal.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// True if the run exceeded `RUN_TIMEOUT` and was killed.
    timed_out: bool,
}

fn fresh_dir(label: &str) -> PathBuf {
    static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let unique = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    let name = format!("silt_wave1_channels_{pid}_{unique}_{label}");
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn read_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.replace("\r\n", "\n")
}

/// Write `src` into a fresh directory and run `silt run` on it once.
///
/// Output goes to files outside the program's directory rather than to
/// pipes, so a child that is killed on timeout cannot leave the test
/// blocked on a read.
fn run_program(label: &str, src: &str) -> Outcome {
    let dir = fresh_dir(label);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("create work dir");
    let program = work.join("main.silt");
    std::fs::write(&program, src).expect("write program");
    let out_path = dir.join("stdout.txt");
    let err_path = dir.join("stderr.txt");
    let out_file = std::fs::File::create(&out_path).expect("create stdout file");
    let err_file = std::fs::File::create(&err_path).expect("create stderr file");

    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&program)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        .spawn()
        .expect("spawn silt");

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if started.elapsed() >= RUN_TIMEOUT => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().expect("wait after kill");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    let outcome = Outcome {
        code: status.code(),
        stdout: read_text(&out_path),
        stderr: read_text(&err_path),
        timed_out,
    };
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

// ── H1 ──────────────────────────────────────────────────────────────

/// The documented multiplexing example, in the documented
/// `loop _ = () { ... loop(()) }` form, on the main thread. Which
/// messages it prints before a channel reports `Closed` depends on the
/// order in which the two tasks (the functions of two `stream.from_fn`,
/// which run beside the main thread) run, so only the end is pinned.
/// Passes with and without the fix.
#[test]
fn guard_documented_select_loop_ends_when_a_channel_closes() {
    let src = r#"
import channel
import stream
fn main() {
  let alerts = channel.new(5)
  let logs = channel.new(5)

  let _logger = stream.from_fn { _ ->
    channel.send(logs, "background task done")
    channel.send(logs, "log rotation complete")
    channel.close(logs)
  }

  let _alerter = stream.from_fn { _ ->
    channel.send(alerts, "disk full!")
    channel.close(alerts)
  }

  loop _ = () {
    match channel.select([channel.Recv(alerts), channel.Recv(logs)]) {
      channel.Received(^alerts, Some(msg)) -> {
        println("alert: {msg}")
        loop(())
      }
      channel.Received(^logs, Some(msg)) -> {
        println("log: {msg}")
        loop(())
      }
      channel.Received(_, None) -> {
        println("a channel closed")
        return ()
      }
      _ -> loop(())
    }
  }
}
"#;
    for run in 1..=HANG_REPEATS {
        let out = run_program("guard_select_loop", src);
        let ctx = format!("guard_select_loop, run {run} of {HANG_REPEATS}");
        assert!(!out.timed_out, "{ctx}: the program hung\n{out:#?}");
        assert_eq!(out.code, Some(0), "{ctx}: exit status\n{out:#?}");
        assert!(
            !out.stderr.contains(DEADLOCK),
            "{ctx}: false deadlock report\n{out:#?}"
        );
        assert!(
            out.stdout.ends_with("a channel closed\n"),
            "{ctx}: the loop must end with the closed channel\n{out:#?}"
        );
        for line in out.stdout.lines() {
            assert!(
                line == "a channel closed"
                    || line == "alert: disk full!"
                    || line == "log: background task done"
                    || line == "log: log rotation complete",
                "{ctx}: unexpected line {line:?}\n{out:#?}"
            );
        }
    }
}

// ── Worker stacks under an address-space limit ──────────────────────

/// Workers reserve a 256 MiB stack each. Under an address-space limit
/// (`RLIMIT_AS`, as containers and CI jobs set) that reservation can be
/// refused; the workers then fall back to the default stack and the
/// program still runs, as it did before workers had large stacks.
#[cfg(target_os = "linux")]
#[test]
fn tasks_run_under_an_address_space_limit() {
    use std::os::unix::process::CommandExt;
    let src = r#"
import channel
import task
import list
fn collect(ch, n, acc) {
  match n {
    0 -> acc
    _ -> match channel.receive(ch) {
      Some(v) -> collect(ch, n - 1, acc + v)
      None -> acc
    }
  }
}
fn main() {
  let ch = channel.new(0)
  let ps = 1..50 |> list.map { i -> { -> channel.send(ch, i) } }
  let totals = task.background(ps) { -> task.map([25, 25], 2) { n -> collect(ch, n, 0) } }
  println("total {list.fold(totals, 0) { acc, t -> acc + t }}")
}
"#;
    let dir = fresh_dir("rlimit_as");
    std::fs::create_dir_all(&dir).expect("create dir");
    let program = dir.join("main.silt");
    std::fs::write(&program, src).expect("write program");
    // 800 MB: room for the binary and its 256 MiB main thread, not for
    // several 256 MiB workers besides.
    const LIMIT: libc::rlim_t = 800_000 * 1024;
    let mut command = Command::new(env!("CARGO_BIN_EXE_silt"));
    command.arg("run").arg(&program).stdin(Stdio::null());
    // SAFETY: only an async-signal-safe `setrlimit` runs in the child
    // between fork and exec.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: LIMIT,
                rlim_max: LIMIT,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn silt");
    let started = Instant::now();
    while child.try_wait().expect("try_wait").is_none() {
        if started.elapsed() >= RUN_TIMEOUT {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().expect("collect output");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        (out.status.code(), stdout.as_ref()),
        (Some(0), "total 1275\n"),
        "stderr: {stderr}"
    );
}
