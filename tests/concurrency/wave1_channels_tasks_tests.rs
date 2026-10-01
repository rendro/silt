//! Channels and tasks: lost wake-ups, `task.cancel`, worker stacks,
//! timers in the deadlock detection, failures that nobody joins.
//!
//! Five defects are locked here.
//!
//!   * H1. A buffered channel woke a parked sender only when a receive
//!     found the buffer full. Several receives in a row woke one
//!     sender, and the other senders stayed parked for good: the
//!     worker-pool shape hung. The same class: a `channel.select` that
//!     was woken for one arm and completed another one used up the
//!     wake-up, and a send that raced a close could be accepted after
//!     the receiver had seen `Closed`.
//!   * H2. `task.cancel` could hang the runtime: the task handle ran
//!     the cancel cleanup while it held the lock on the cleanup, and
//!     the cleanup took the lock of the parked task, which the workers
//!     and the wakers take first.
//!   * H3. Scheduler workers had the default stack of 2 MiB, so
//!     recursion through a callback inside `task.spawn` overflowed the
//!     native stack at a small depth and aborted the process.
//!   * H4. A task that waits for a timer was counted as stuck, so a
//!     main thread that waited for that task's send was told
//!     "deadlock".
//!   * H5. A task that failed and was never joined left no trace.
//!
//! Most scenarios are golden cases:
//! `tests/golden/concurrency/*/wave1_channels_tasks__*`. What stays here
//! needs more than the golden format expresses: output whose order
//! depends on scheduling (checked line by line), a count of reports on
//! stderr whose order varies, and a run under `RLIMIT_AS`.
//!
//! Every test runs the built `silt` binary on a program in a fresh
//! temporary directory and asserts on exit status, stdout and stderr.
//! Every run has a kill timeout, so a hang fails the test instead of
//! hanging the suite. The defects are races, so every scenario runs
//! several times: ten times where the defect is a hang, three times
//! otherwise. The assertions are on results, never on elapsed time.
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

/// How often every other scenario runs.
const REPEATS: usize = 3;

/// Every main-thread deadlock diagnostic starts with this.
const DEADLOCK: &str = "deadlock on main thread";

/// The first line of such a report, once per report. (The report shows
/// the source line of the failure, and repeats its message under it.)
const REPORT_HEADER: &str = "error[runtime]: task <handle:";

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

/// In each of `repeats` runs the program must run to its end, report
/// no deadlock, print exactly `expected`, and exit with status 1
/// because a task failed and nobody joined it. Returns the outcomes,
/// for further assertions.
fn assert_completes_with_unjoined_failure(
    label: &str,
    src: &str,
    expected: &str,
    repeats: usize,
) -> Vec<Outcome> {
    let mut outcomes = Vec::with_capacity(repeats);
    for run in 1..=repeats {
        let out = run_program(label, src);
        let ctx = format!("{label}, run {run} of {repeats}");
        assert!(
            !out.timed_out,
            "{ctx}: the program hung and was killed after {RUN_TIMEOUT:?}\n{out:#?}"
        );
        assert!(
            !out.stderr.contains(DEADLOCK),
            "{ctx}: the program was told `{DEADLOCK}`, but it can complete\n{out:#?}"
        );
        assert_eq!(
            out.code,
            Some(1),
            "{ctx}: a task failed unjoined, so the program must exit with status 1\n{out:#?}"
        );
        assert_eq!(
            out.stdout, expected,
            "{ctx}: the program printed something else than expected\n{out:#?}"
        );
        outcomes.push(out);
    }
    outcomes
}

// ── H1 ──────────────────────────────────────────────────────────────

/// The documented multiplexing example, in the documented
/// `loop _ = () { ... loop(()) }` form, on the main thread. Which
/// messages it prints before a channel reports `Closed` depends on the
/// order in which the two tasks run, so only the end is pinned. Passes
/// with and without the fix.
#[test]
fn guard_documented_select_loop_ends_when_a_channel_closes() {
    let src = r#"
import channel
import task
fn main() {
  let alerts = channel.new(5)
  let logs = channel.new(5)

  let _ = task.spawn({ ->
    channel.send(logs, "background task done")
    channel.send(logs, "log rotation complete")
    channel.close(logs)
  })

  let _ = task.spawn({ ->
    channel.send(alerts, "disk full!")
    channel.close(alerts)
  })

  loop _ = () {
    match channel.select([Recv(alerts), Recv(logs)]) {
      (^alerts, Message(msg)) -> {
        println("alert: {msg}")
        loop(())
      }
      (^logs,   Message(msg)) -> {
        println("log: {msg}")
        loop(())
      }
      (_, Closed) -> {
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

// ── H5 ──────────────────────────────────────────────────────────────

/// Every task that failed and was not joined gets its report.
#[test]
fn h5_every_unjoined_failure_is_reported() {
    let src = r#"
import channel
import task
import time
fn main() {
  let about_to_fail = channel.new(2)
  let _ = task.spawn({ ->
    channel.send(about_to_fail, 1)
    panic("first worker failed")
  })
  let _ = task.spawn({ ->
    channel.send(about_to_fail, 2)
    panic("second worker failed")
  })
  let fine = task.spawn({ -> 42 })
  let _ = channel.receive(about_to_fail)
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  println("main done {task.join(fine)}")
}
"#;
    let outcomes =
        assert_completes_with_unjoined_failure("h5_two_failures", src, "main done 42\n", REPEATS);
    for out in outcomes {
        assert!(
            out.stderr.contains("first worker failed"),
            "the first failure must be reported\n{out:#?}"
        );
        assert!(
            out.stderr.contains("second worker failed"),
            "the second failure must be reported\n{out:#?}"
        );
        assert_eq!(
            out.stderr.matches(REPORT_HEADER).count(),
            2,
            "two failed tasks, two reports\n{out:#?}"
        );
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
      Message(v) -> collect(ch, n - 1, acc + v)
      _ -> acc
    }
  }
}
fn main() {
  let ch = channel.new()
  let ps = 1..50 |> list.map { i -> task.spawn({ -> channel.send(ch, i) }) }
  let r = task.spawn({ -> collect(ch, 25, 0) })
  let m = collect(ch, 25, 0)
  let t = task.join(r)
  println("total {m + t}")
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
