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

/// In every run the program must finish in time, report no deadlock,
/// exit with status 0 and print exactly `expected`.
fn assert_completes(label: &str, src: &str, expected: &str) {
    for run in 1..=REPEATS {
        let out = run_program(label, src);
        let ctx = format!("{label}, run {run} of {REPEATS}");
        assert!(
            !out.timed_out,
            "{ctx}: the program hung and was killed after {RUN_TIMEOUT:?}\n{out:#?}"
        );
        assert!(
            !out.stderr.contains(DEADLOCK),
            "{ctx}: false deadlock report on a receive from a stream that was \
             still going to deliver or close\n{out:#?}"
        );
        assert_eq!(
            out.code,
            Some(0),
            "{ctx}: the program must exit with status 0\n{out:#?}"
        );
        assert_eq!(
            out.stdout, expected,
            "{ctx}: the program printed something else than expected\n{out:#?}"
        );
    }
}

/// In every run the program must end with the main-thread deadlock
/// diagnostic: in time (a hang is a failure), with a non-zero exit
/// status, after printing exactly `expected`.
fn assert_reports_deadlock(label: &str, src: &str, expected: &str) {
    for run in 1..=REPEATS {
        let out = run_program(label, src);
        let ctx = format!("{label}, run {run} of {REPEATS}");
        assert!(
            !out.timed_out,
            "{ctx}: the program hung instead of reporting the deadlock; \
             killed after {RUN_TIMEOUT:?}\n{out:#?}"
        );
        assert!(
            out.stderr.contains(DEADLOCK),
            "{ctx}: expected the `{DEADLOCK}` diagnostic on stderr\n{out:#?}"
        );
        assert_ne!(
            out.code,
            Some(0),
            "{ctx}: a deadlocked program must exit with a non-zero status\n{out:#?}"
        );
        assert_eq!(
            out.stdout, expected,
            "{ctx}: the program printed something else than expected\n{out:#?}"
        );
    }
}

// ── A main-thread receive on a stream completes ──────────────────────
//
// Every test in this group fails without the fix: the receive is
// reported as a deadlock.

/// `channel.each` on the main thread over a stream with a slow stage,
/// in a program with no `task.spawn`, so no scheduler ever exists. The
/// stage sleeps before every element, so the channel is empty each time
/// the main thread looks.
#[test]
fn main_thread_each_over_a_slow_stream_completes() {
    let src = r#"
import channel
import stream
import time

fn main() {
  let out = stream.from_list([1, 2, 3]) |> stream.map { x ->
    time.sleep(time.ms(300))
    x * 10
  }
  channel.each(out) { v -> println(v) }
  println("done")
}
"#;
    assert_completes("each_slow_stream", src, "10\n20\n30\ndone\n");
}

/// `channel.receive` on the main thread on such a stream receives the
/// first value.
#[test]
fn main_thread_receive_on_a_slow_stream_gets_the_first_value() {
    let src = r#"
import channel
import stream
import time

fn main() {
  let out = stream.from_list([1, 2, 3]) |> stream.map { x ->
    time.sleep(time.ms(300))
    x * 10
  }
  println(channel.receive(out))
}
"#;
    assert_completes("receive_slow_stream", src, "Message(10)\n");
}

/// The same without any slow stage: a plain source has not pushed its
/// first element yet when the main thread receives right after creating
/// it. This is the race against the start of the stream thread.
#[test]
fn main_thread_receive_on_a_plain_source_gets_the_first_value() {
    let src = r#"
import channel
import stream

fn main() {
  let out = stream.from_list([7, 8, 9])
  println(channel.receive(out))
}
"#;
    assert_completes("receive_plain_source", src, "Message(7)\n");
}

/// A stage that ends without ever delivering: the receive must see
/// `Closed`.
#[test]
fn main_thread_receive_on_a_stream_that_delivers_nothing_gets_closed() {
    let src = r#"
import channel
import stream
import time

fn main() {
  let out = stream.from_list([1, 2]) |> stream.filter { x ->
    time.sleep(time.ms(200))
    false
  }
  println(channel.receive(out))
  println("end")
}
"#;
    assert_completes("receive_closed_stream", src, "Closed\nend\n");
}

/// `channel.select` on the main thread, no scheduler. One arm is a
/// stream, the other a channel that nobody ever serves: the stream arm
/// alone must keep the select from being reported.
#[test]
fn main_thread_select_with_a_stream_arm_completes() {
    let src = r#"
import channel
import stream
import time

fn main() {
  let idle = channel.new(0)
  let out = stream.from_list(["a", "b"]) |> stream.map { x ->
    time.sleep(time.ms(300))
    x
  }
  match channel.select([Recv(idle), Recv(out)]) {
    (_, Message(v)) -> println("first {v}")
    (_, Closed) -> println("closed")
    _ -> println("other")
  }
  match channel.select([Recv(out), Recv(idle)]) {
    (_, Message(v)) -> println("second {v}")
    (_, Closed) -> println("closed")
    _ -> println("other")
  }
}
"#;
    assert_completes("select_stream_arm", src, "first a\nsecond b\n");
}

/// `channel.select` on the main thread over a slow stream after a task
/// has been spawned and joined. A scheduler exists then, and the
/// verdict would come from the wake graph, which cannot see stream
/// threads either. The stage sleeps longer than the 200 ms confirmation
/// window of the detector, so a false verdict has time to fire.
#[test]
fn main_thread_select_on_a_slow_stream_with_a_scheduler_completes() {
    let src = r#"
import channel
import stream
import task
import time

fn main() {
  task.join(task.spawn(fn() { 1 }))
  let out = stream.from_list([1, 2, 3]) |> stream.map { x ->
    time.sleep(time.ms(300))
    x * 10
  }
  match channel.select([Recv(out)]) {
    (_, Message(v)) -> println("got {v}")
    (_, Closed) -> println("closed")
    _ -> println("other")
  }
}
"#;
    assert_completes("select_with_scheduler", src, "got 10\n");
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

// ── A real deadlock is still reported ────────────────────────────────

/// No streams at all: a receive on a fresh channel with no producer and
/// no `task.spawn` is reported as before.
#[test]
fn receive_without_any_stream_still_reports_the_deadlock() {
    let src = r#"
import channel

fn main() {
  let ch = channel.new(0)
  channel.receive(ch)
  println("unreachable")
}
"#;
    assert_reports_deadlock("no_stream", src, "");
}

/// After a stream that the main thread consumed to its end with
/// `channel.each`, a receive on a different, fresh channel is reported.
#[test]
fn receive_after_a_fully_consumed_stream_still_reports_the_deadlock() {
    let src = r#"
import channel
import stream
import time

fn main() {
  let out = stream.from_list([1, 2, 3]) |> stream.map { x ->
    time.sleep(time.ms(100))
    x * 10
  }
  channel.each(out) { v -> println(v) }
  println("stream done")
  let fresh = channel.new(0)
  channel.receive(fresh)
  println("unreachable")
}
"#;
    assert_reports_deadlock("after_stream", src, "10\n20\n30\nstream done\n");
}

// ── ... also while stage threads are still alive ─────────────────────
//
// In each program below at least one stage thread outlives the
// pipeline: it is blocked on a buffer that nobody reads any more. The
// deadlock is on a fresh channel that has nothing to do with any
// stream, and must be reported exactly as if no stream had ever
// existed. These tests pass without the fix; they guard against a fix
// that lets live stream threads suppress the detection.

/// The pipeline from the documentation. `take(5)` ends after five
/// elements and leaves the source, the filter and the map alive.
#[test]
fn deadlock_is_reported_after_the_documented_pipeline_with_take() {
    let src = r#"
import channel
import stream

fn main() {
  let squares = stream.from_range(1, 100)
    |> stream.filter(fn(n) { n % 2 == 1 })
    |> stream.map(fn(n) { n * n })
    |> stream.take(5)
    |> stream.collect
  println(squares)
  let fresh = channel.new(0)
  channel.receive(fresh)
  println("unreachable")
}
"#;
    assert_reports_deadlock("after_doc_pipeline", src, "[1, 9, 25, 49, 81]\n");
}

/// An infinite source cut short by `take`.
#[test]
fn deadlock_is_reported_after_repeat_take_collect() {
    let src = r#"
import channel
import stream

fn main() {
  println(stream.repeat("x") |> stream.take(3) |> stream.collect)
  let fresh = channel.new(0)
  channel.receive(fresh)
  println("unreachable")
}
"#;
    assert_reports_deadlock("after_repeat_take", src, "[x, x, x]\n");
}

/// `zip` of 3 elements against 1000: it ends with the shorter input and
/// leaves the longer source alive.
#[test]
fn deadlock_is_reported_after_zip_of_unequal_lengths() {
    let src = r#"
import channel
import stream

fn main() {
  let short = stream.from_list([1, 2, 3])
  let long = stream.from_range(1, 1000)
  println(stream.zip(short, long) |> stream.collect)
  let fresh = channel.new(0)
  channel.receive(fresh)
  println("unreachable")
}
"#;
    assert_reports_deadlock("after_zip", src, "[(1, 1), (2, 2), (3, 3)]\n");
}

/// A finite source that is never read: 100 elements do not fit its
/// buffer, so its thread stays alive.
#[test]
fn deadlock_is_reported_after_an_unconsumed_source() {
    let src = r#"
import channel
import stream

fn main() {
  let never_read = stream.from_range(1, 100)
  println("before")
  let fresh = channel.new(0)
  channel.receive(fresh)
  println("unreachable")
}
"#;
    assert_reports_deadlock("after_unconsumed_source", src, "before\n");
}

/// The same for `channel.send`: the fix does not touch sends at all.
#[test]
fn send_deadlock_is_reported_while_a_stage_thread_is_alive() {
    let src = r#"
import channel
import stream

fn main() {
  println(stream.repeat("x") |> stream.take(3) |> stream.collect)
  let fresh = channel.new(0)
  channel.send(fresh, 1)
  println("unreachable")
}
"#;
    assert_reports_deadlock("send_after_repeat_take", src, "[x, x, x]\n");
}

/// The same for a `channel.select` that has no stream arm.
#[test]
fn select_deadlock_is_reported_while_a_stage_thread_is_alive() {
    let src = r#"
import channel
import stream

fn main() {
  println(stream.repeat("x") |> stream.take(3) |> stream.collect)
  let a = channel.new(0)
  let b = channel.new(0)
  match channel.select([Recv(a), Send(b, 1)]) {
    _ -> println("unreachable")
  }
}
"#;
    assert_reports_deadlock("select_after_repeat_take", src, "[x, x, x]\n");
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

/// One file: a stream test, then a test that deadlocks on a fresh
/// channel, then a trivial test. All three run in the same VM, so the
/// stage threads of the first test are alive during the second.
#[test]
fn silt_test_reports_a_deadlock_that_follows_a_stream_test() {
    let header = "import channel\nimport stream\nimport test\n";
    let file = format!("{header}{STREAM_TEST}{DEADLOCK_TESTS}");
    let files = [("stream_then_deadlock_test.silt", file.as_str())];
    assert_silt_test_reports_the_deadlock("silt_test_one_file", &files, files[0].0);
}

/// The same split over two files of one `silt test` run. Each file gets
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
