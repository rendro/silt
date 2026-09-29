//! Regression locks: main-thread channel operations against `stream.*`
//! stages.
//!
//! Stream sources and transforms run on plain OS threads, not on
//! scheduler tasks. The main-thread waits behind `channel.receive`,
//! `channel.each` and `channel.select` could not see those threads, so
//! a receive on the output of a stage that was about to deliver was
//! reported as "deadlock on main thread".
//!
//! The fix records the output channel of every stream stage. A receive
//! on a recorded channel gets no deadlock verdict: it waits for a value
//! or for `Closed`. Nothing else changes. In particular a wait on any
//! OTHER channel gets the same verdict as before, whether or not stream
//! threads are alive.
//!
//! These tests lock both halves:
//!
//!   * a main-thread receive on a stream completes;
//!   * a real deadlock on a channel that is not a stream is reported,
//!     also while stage threads are still alive. Stage threads commonly
//!     outlive their pipeline (a truncating stage such as `stream.take`
//!     leaves its upstream stages blocked on a full buffer for the rest
//!     of the program), and an earlier version of this fix let any live
//!     stage thread switch the detection off: every program in the
//!     second group hung instead of reporting.
//!
//! Most scenarios live as golden cases
//! (`tests/golden/concurrency/{streams,deadlock,silt_test}/stream_channel_main_thread_wait__*`).
//! What stays here: the two failing-stage tests, which deliberately do
//! not pin the exit status (the golden harness always does), and the
//! two-file `silt test` run over a directory.
//!
//! Every test runs the built `silt` binary on files in a fresh
//! temporary directory and asserts on its exit status and output. Each
//! run has a timeout, so a hang fails the test instead of hanging the
//! suite. The defect is a race between the main thread and a stream
//! thread, so each scenario runs several times, and the assertions are
//! on results only, never on elapsed time.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Upper bound for one run of the binary. A run that exceeds it is
/// killed and reported as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

/// How often each scenario is run.
const REPEATS: usize = 3;

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
    let name = format!("silt_stream_main_wait_{pid}_{unique}_{label}");
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn read_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.replace("\r\n", "\n")
}

/// Write `files` (name, content) into a fresh directory and run
/// `silt <subcommand> <target>` once. `target` is the name of one of
/// the files, or "" for the directory itself.
///
/// Output goes to files outside that directory rather than to pipes,
/// so a child that is killed on timeout cannot leave the test blocked
/// on a read.
fn run_silt(label: &str, subcommand: &str, files: &[(&str, &str)], target: &str) -> Outcome {
    let dir = fresh_dir(label);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("create work dir");
    for (name, content) in files {
        std::fs::write(work.join(name), content).expect("write source file");
    }
    let target_path = if target.is_empty() {
        work.clone()
    } else {
        work.join(target)
    };
    let out_path = dir.join("stdout.txt");
    let err_path = dir.join("stderr.txt");
    let out_file = std::fs::File::create(&out_path).expect("create stdout file");
    let err_file = std::fs::File::create(&err_path).expect("create stderr file");

    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(subcommand)
        .arg(&target_path)
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

/// `silt run` on a single program.
fn run_program(label: &str, src: &str) -> Outcome {
    run_silt(label, "run", &[("main.silt", src)], "main.silt")
}

// ── A failing stage must not hang the main thread ────────────────────
//
// The callback of a stage fails while the main thread waits on the
// stage's output.
//
// What happens today: the stage swallows the callback's error, closes
// its output and ends; the main thread sees `Closed`. The elements
// before the failing one are delivered, the rest are dropped without a
// diagnostic, and the program exits with status 0. That the error is
// swallowed is a known, separate defect. These tests therefore do not
// pin the exit status or what is printed after the failure; they pin
// what must hold either way: the run ends, the elements before the
// failure arrive, and the failure is not reported as a deadlock.

fn assert_failing_stage_does_not_hang(label: &str, src: &str) {
    for run in 1..=REPEATS {
        let out = run_program(label, src);
        let ctx = format!("{label}, run {run} of {REPEATS}");
        assert!(
            !out.timed_out,
            "{ctx}: the main thread hung on the output of a failed stage; \
             killed after {RUN_TIMEOUT:?}\n{out:#?}"
        );
        assert!(
            out.stdout.starts_with("100\n50\n"),
            "{ctx}: the elements before the failing one must be delivered\n{out:#?}"
        );
        assert!(
            !out.stderr.contains(DEADLOCK),
            "{ctx}: a failed stage must not be reported as a deadlock\n{out:#?}"
        );
    }
}

/// The callback hits a runtime error (division by zero) on the third
/// element.
#[test]
fn stream_callback_error_does_not_hang_the_main_thread() {
    let src = r#"
import channel
import stream
import time

fn main() {
  let out = stream.from_list([1, 2, 0, 4]) |> stream.map { x ->
    time.sleep(time.ms(100))
    100 / x
  }
  channel.each(out) { v -> println(v) }
  println("after each")
}
"#;
    assert_failing_stage_does_not_hang("callback_error", src);
}

/// The callback calls `panic` on the third element.
#[test]
fn stream_callback_panic_does_not_hang_the_main_thread() {
    let src = r#"
import channel
import stream
import time

fn main() {
  let out = stream.from_list([1, 2, 0, 4]) |> stream.map { x ->
    time.sleep(time.ms(100))
    match x {
      0 -> panic("boom")
      _ -> 100 / x
    }
  }
  channel.each(out) { v -> println(v) }
  println("after each")
}
"#;
    assert_failing_stage_does_not_hang("callback_panic", src);
}

// ── `silt test` ──────────────────────────────────────────────────────

const STREAM_TEST: &str = r#"
fn test_a_stream_pipeline() {
  let squares = stream.from_range(1, 100)
    |> stream.filter(fn(n) { n % 2 == 1 })
    |> stream.map(fn(n) { n * n })
    |> stream.take(5)
    |> stream.collect
  test.assert_eq(squares, [1, 9, 25, 49, 81])
}
"#;

const DEADLOCK_TESTS: &str = r#"
fn test_b_real_deadlock() {
  let fresh = channel.new(0)
  let _ = channel.receive(fresh)
  test.assert(false, "unreachable")
}

fn test_c_trivial() {
  test.assert_eq(1 + 1, 2)
}
"#;

/// Number of lines in a `silt test` report that start with `verdict`.
fn count_verdicts(report: &str, verdict: &str) -> usize {
    report
        .lines()
        .filter(|line| line.trim_start().starts_with(verdict))
        .count()
}

/// In every run `silt test` must finish in time and report the three
/// tests as two passes and one failure, the failure being the deadlock.
fn assert_silt_test_reports_the_deadlock(label: &str, files: &[(&str, &str)], target: &str) {
    for run in 1..=REPEATS {
        let out = run_silt(label, "test", files, target);
        let ctx = format!("{label}, run {run} of {REPEATS}");
        let report = format!("{}{}", out.stdout, out.stderr);
        assert!(
            !out.timed_out,
            "{ctx}: `silt test` hung on the deadlocking test instead of failing it; \
             killed after {RUN_TIMEOUT:?}\n{out:#?}"
        );
        assert_ne!(
            out.code,
            Some(0),
            "{ctx}: `silt test` must exit with a non-zero status, a test failed\n{out:#?}"
        );
        assert!(
            report.contains(DEADLOCK),
            "{ctx}: the failure must be the `{DEADLOCK}` diagnostic\n{out:#?}"
        );
        assert_eq!(
            count_verdicts(&report, "FAIL "),
            1,
            "{ctx}: exactly one test must fail, the deadlocking one\n{out:#?}"
        );
        assert_eq!(
            count_verdicts(&report, "PASS "),
            2,
            "{ctx}: the stream test and the test after the deadlock must pass\n{out:#?}"
        );
    }
}

/// A stream test, then a test that deadlocks on a fresh channel, then a
/// trivial test, split over two files of one `silt test` run over the
/// directory (the one-file variant is the golden case
/// `tests/golden/concurrency/silt_test/stream_channel_main_thread_wait__*`;
/// the golden harness cannot point `silt test` at a directory). Each file gets
/// its own VM, and channel ids restart at 0 in each: the fresh channel
/// of the second file has the id of a stream channel of the first. It
/// must not be taken for one.
#[test]
fn silt_test_reports_a_deadlock_in_a_file_that_follows_a_stream_file() {
    let stream_file = format!("import stream\nimport test\n{STREAM_TEST}");
    let deadlock_file = format!("import channel\nimport test\n{DEADLOCK_TESTS}");
    let files = [
        ("a_stream_test.silt", stream_file.as_str()),
        ("b_deadlock_test.silt", deadlock_file.as_str()),
    ];
    assert_silt_test_reports_the_deadlock("silt_test_two_files", &files, "");
}
