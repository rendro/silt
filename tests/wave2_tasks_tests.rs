//! Tasks that fail and that nobody joins: exit status, attribution to
//! the test that spawned them, rendering, `task.cancel`, the REPL.
//!
//!   * `silt run` reported such a failure on stderr but exited 0.
//!     It now exits 1 when a task failed and was neither joined nor
//!     cancelled by the time the program ends.
//!   * `task.cancel` on a task that had failed left the failure to be
//!     reported. Cancelling now counts as handling the task.
//!   * The report showed ` --> <input>:L:C` and a call stack of
//!     `line N, column M`. It now names the program's file and shows the
//!     source line, like every other runtime error.
//!   * `silt test` printed the report under whichever test happened to
//!     be running, or after the summary, and passed the test that spawned
//!     the task. The failure now fails the test that spawned the task,
//!     also when it happens after that test returned, and the report is
//!     indented under it.
//!   * The REPL reported such failures only when the session ended. It
//!     now reports them after the input during which they happened.
//!
//! Every test runs the built `silt` binary on files in a fresh temporary
//! directory and asserts on exit status, stdout and stderr. Each run has
//! a kill timeout, so a hang fails the test instead of hanging the
//! suite. Where the outcome depends on when a task fails, the task and
//! the program hand off over a channel first, the scenario runs three
//! times, and the assertions are on results only, never on time.
//!
//! Tests named `guard_*` pin behaviour that must not change; they pass
//! with and without the fix. All other tests fail without it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Upper bound for one run of the binary. A run that exceeds it is
/// killed and counts as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

/// How often each scenario runs.
const REPEATS: usize = 3;

/// Part of every report of a task that failed and was not joined.
const NEVER_JOINED: &str = "failed and was never joined";

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
    let name = format!("silt_wave2_tasks_{pid}_{unique}_{label}");
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn read_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.replace("\r\n", "\n")
}

/// Write `file_name` with `src` into a fresh directory and run
/// `silt <args...>` there, with that directory as the working
/// directory, so the program is named by its relative file name. With
/// `stdin`, the text is the process's standard input.
///
/// Output goes to files outside the program's directory rather than to
/// pipes, so a child that is killed on timeout cannot leave the test
/// blocked on a read.
fn run_silt(
    label: &str,
    file_name: &str,
    src: &str,
    args: &[&str],
    stdin: Option<&str>,
) -> Outcome {
    let dir = fresh_dir(label);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("create work dir");
    std::fs::write(work.join(file_name), src).expect("write program");
    let out_path = dir.join("stdout.txt");
    let err_path = dir.join("stderr.txt");
    let out_file = std::fs::File::create(&out_path).expect("create stdout file");
    let err_file = std::fs::File::create(&err_path).expect("create stderr file");
    let stdin = match stdin {
        Some(text) => {
            let in_path = dir.join("stdin.txt");
            std::fs::write(&in_path, text).expect("write stdin");
            Stdio::from(std::fs::File::open(&in_path).expect("open stdin"))
        }
        None => Stdio::null(),
    };

    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(args)
        .current_dir(&work)
        .env("NO_COLOR", "1")
        .env("SILT_HISTORY_FILE", dir.join("history"))
        .stdin(stdin)
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

/// `silt run main.silt`, run `REPEATS` times; every run must finish in
/// time. Returns the outcomes.
fn run_program(label: &str, src: &str) -> Vec<Outcome> {
    (1..=REPEATS)
        .map(|run| {
            let out = run_silt(label, "main.silt", src, &["run", "main.silt"], None);
            assert!(
                !out.timed_out,
                "{label}, run {run} of {REPEATS}: the program hung and was killed after \
                 {RUN_TIMEOUT:?}\n{out:#?}"
            );
            out
        })
        .collect()
}

/// `silt test tasks_test.silt`, run `REPEATS` times; every run must
/// finish in time. Returns the outcomes.
fn run_tests(label: &str, src: &str) -> Vec<Outcome> {
    (1..=REPEATS)
        .map(|run| {
            let out = run_silt(
                label,
                "tasks_test.silt",
                src,
                &["test", "tasks_test.silt"],
                None,
            );
            assert!(
                !out.timed_out,
                "{label}, run {run} of {REPEATS}: `silt test` hung and was killed after \
                 {RUN_TIMEOUT:?}\n{out:#?}"
            );
            out
        })
        .collect()
}

/// The line of `stderr` that starts (after indentation) with `prefix`.
fn line_starting_with<'a>(stderr: &'a str, prefix: &str) -> Option<(usize, &'a str)> {
    stderr
        .lines()
        .enumerate()
        .find(|(_, line)| line.trim_start().starts_with(prefix))
}

// ── silt run ─────────────────────────────────────────────────────────

/// A task fails, nobody joins it, `main` returns normally: the failure
/// is reported and the run exits with status 1.
///
/// The task tells `main` that it is about to fail, and fails in its
/// next step; `main` sleeps after that message, so the failure has
/// happened when `main` returns.
#[test]
fn run_exits_1_when_a_task_failed_and_was_never_joined() {
    let src = r#"import channel
import task
import time
fn main() {
  let about_to_fail = channel.new(1)
  let _ = task.spawn(fn() {
    channel.send(about_to_fail, 1)
    let zero = 0
    10 / zero
  })
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  println("main done")
}
"#;
    for out in run_program("run_exit_1", src) {
        assert_eq!(
            out.code,
            Some(1),
            "an unjoined failure fails the run\n{out:#?}"
        );
        assert_eq!(out.stdout, "main done\n", "main ran to its end\n{out:#?}");
        assert_eq!(
            out.stderr.matches(REPORT_HEADER).count(),
            1,
            "one failed task, one report\n{out:#?}"
        );
        assert!(
            out.stderr.contains("division by zero"),
            "the task's error\n{out:#?}"
        );
    }
}

/// The report names the program's file, shows the source line of the
/// failure, and gives a call stack whose frames name the file too.
#[test]
fn run_report_names_the_file_with_source_line_and_call_stack() {
    let src = r#"import task
import time
fn inner(n) { 10 / n }
fn outer(n) { inner(n) + 1 }
fn main() {
  let _ = task.spawn(fn() { outer(0) })
  time.sleep(time.ms(300))
  println("main done")
}
"#;
    for out in run_program("run_report_location", src) {
        assert!(
            !out.stderr.contains("<input>"),
            "the report must name the file, not `<input>`\n{out:#?}"
        );
        assert!(
            out.stderr.contains(" --> main.silt:3:"),
            "the report must point into main.silt at the failing line\n{out:#?}"
        );
        assert!(
            out.stderr.contains("fn inner(n) { 10 / n }"),
            "the report must show the source line\n{out:#?}"
        );
        assert!(
            out.stderr.contains("-> inner  at main.silt:3:")
                && out.stderr.contains("-> outer  at main.silt:4:"),
            "the call stack frames must name the file\n{out:#?}"
        );
        assert!(
            out.stderr.contains("= help: join the task with task.join"),
            "the report must say how to handle the failure\n{out:#?}"
        );
        assert_eq!(out.code, Some(1), "{out:#?}");
    }
}

/// The failures are reported before the error of `main`, and the run
/// fails once, with status 1.
#[test]
fn run_reports_task_failure_and_main_error() {
    let src = r#"import channel
import task
import time
fn main() {
  let about_to_fail = channel.new(1)
  let _ = task.spawn(fn() {
    channel.send(about_to_fail, 1)
    panic("worker failed")
  })
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  panic("main failed")
}
"#;
    for out in run_program("run_both_fail", src) {
        assert_eq!(out.code, Some(1), "{out:#?}");
        let report = out.stderr.find("worker failed").expect("the task's report");
        let main_error = out.stderr.find("main failed").expect("main's error");
        assert!(
            report < main_error,
            "the task's report comes first\n{out:#?}"
        );
        assert!(
            !out.stderr.contains("<input>"),
            "the report must name the file\n{out:#?}"
        );
    }
}

/// `task.cancel` on a task that has failed handles the failure: no
/// report, exit status 0.
#[test]
fn cancel_after_failure_dismisses_the_failure() {
    let src = r#"import channel
import task
import time
fn main() {
  let about_to_fail = channel.new(1)
  let h = task.spawn(fn() {
    channel.send(about_to_fail, 1)
    let zero = 0
    10 / zero
  })
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  task.cancel(h)
  println("done")
}
"#;
    for out in run_program("cancel_after_failure", src) {
        assert!(
            !out.stderr.contains(NEVER_JOINED),
            "a cancelled task's failure is not reported\n{out:#?}"
        );
        assert_eq!(
            out.code,
            Some(0),
            "a cancelled failure does not fail the run\n{out:#?}"
        );
        assert_eq!(out.stdout, "done\n", "{out:#?}");
    }
}

/// A join after the cancel still raises the task's own error: the
/// handle keeps the result that came first. Passes with and without
/// the fix.
#[test]
fn guard_join_after_cancel_of_a_failed_task_raises_the_task_error() {
    let src = r#"import channel
import task
import time
fn main() {
  let about_to_fail = channel.new(1)
  let h = task.spawn(fn() {
    channel.send(about_to_fail, 1)
    panic("worker failed")
  })
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  task.cancel(h)
  let v = task.join(h)
  println("not reached {v}")
}
"#;
    for out in run_program("guard_join_after_cancel", src) {
        assert_eq!(out.code, Some(1), "the join raises\n{out:#?}");
        assert_eq!(out.stdout, "", "{out:#?}");
        assert!(
            out.stderr.contains("joined task failed") && out.stderr.contains("worker failed"),
            "the join raises the task's own error\n{out:#?}"
        );
        assert!(
            !out.stderr.contains(NEVER_JOINED),
            "a joined failure is not reported as unjoined\n{out:#?}"
        );
    }
}

/// A task that is still running when `main` returns is not a failure,
/// even if it would fail later. Passes with and without the fix.
#[test]
fn guard_task_still_running_at_the_end_is_not_a_failure() {
    let src = r#"import task
import time
fn main() {
  let _ = task.spawn(fn() {
    time.sleep(time.ms(3000))
    let zero = 0
    10 / zero
  })
  println("main done")
}
"#;
    for out in run_program("guard_still_running", src) {
        assert_eq!(out.code, Some(0), "{out:#?}");
        assert_eq!(out.stdout, "main done\n", "{out:#?}");
        assert!(!out.stderr.contains(NEVER_JOINED), "{out:#?}");
    }
}

// ── silt test ────────────────────────────────────────────────────────

/// A task that fails while its test runs fails that test. The report is
/// indented under the test's result line and names the file.
#[test]
fn test_task_failure_during_the_test_fails_that_test() {
    let src = r#"import channel
import task
import test
import time

fn test_spawns_failing_task() {
  let about_to_fail = channel.new(1)
  let _ = task.spawn(fn() {
    channel.send(about_to_fail, 1)
    let zero = 0
    10 / zero
  })
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  test.assert_eq(1, 1)
}

fn test_other() {
  test.assert_eq(2, 2)
}
"#;
    for out in run_tests("test_during", src) {
        assert_eq!(out.code, Some(1), "{out:#?}");
        let (fail_at, _) = line_starting_with(
            &out.stderr,
            "FAIL tasks_test.silt::test_spawns_failing_task",
        )
        .unwrap_or_else(|| panic!("the spawning test must fail\n{out:#?}"));
        let (report_at, report) = line_starting_with(&out.stderr, "error[runtime]: task <handle:")
            .unwrap_or_else(|| panic!("the task's failure must be reported\n{out:#?}"));
        assert!(
            fail_at < report_at,
            "the report comes under the result line\n{out:#?}"
        );
        assert!(
            report.starts_with("    error[runtime]:"),
            "the report is indented under the test\n{out:#?}"
        );
        assert!(
            out.stderr.contains("     --> tasks_test.silt:11:"),
            "the report names the file and the failing line\n{out:#?}"
        );
        assert!(
            out.stderr.contains("PASS tasks_test.silt::test_other"),
            "the other test passes\n{out:#?}"
        );
        assert!(
            out.stderr
                .contains("2 tests: 1 passed, 1 failed, 0 skipped"),
            "the summary counts the spawning test as failed\n{out:#?}"
        );
    }
}

/// A task that fails after its test has returned, while a later test
/// runs, fails the test that spawned it, not the one that runs. The
/// summary and the exit status count it.
///
/// The later test waits until the task is about to fail, then sleeps,
/// so the failure happens during the later test.
#[test]
fn test_late_task_failure_fails_the_test_that_spawned_it() {
    let src = r#"import channel
import task
import test
import time

let about_to_fail = channel.new(1)

fn test_a_spawns_late_failure() {
  let _ = task.spawn(fn() {
    time.sleep(time.ms(100))
    channel.send(about_to_fail, 1)
    let zero = 0
    10 / zero
  })
  test.assert_eq(1, 1)
}

fn test_b_waits() {
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  test.assert_eq(2, 2)
}

fn test_c() {
  test.assert_eq(3, 3)
}
"#;
    for out in run_tests("test_late", src) {
        assert_eq!(
            out.code,
            Some(1),
            "the late failure fails the run\n{out:#?}"
        );
        assert!(
            out.stderr.contains(
                "FAIL tasks_test.silt::test_a_spawns_late_failure (a task it spawned failed \
                 after the test had returned)"
            ),
            "the spawning test is reported failed\n{out:#?}"
        );
        assert!(
            out.stderr.contains("PASS tasks_test.silt::test_b_waits")
                && !out.stderr.contains("FAIL tasks_test.silt::test_b_waits"),
            "the test that happened to run passes\n{out:#?}"
        );
        assert!(
            out.stderr.contains("PASS tasks_test.silt::test_c"),
            "{out:#?}"
        );
        assert!(
            out.stderr
                .contains("3 tests: 2 passed, 1 failed, 0 skipped"),
            "the summary counts the spawning test as failed\n{out:#?}"
        );
        let report = out.stderr.find(NEVER_JOINED).expect("the report");
        let summary = out.stderr.find("3 tests:").expect("the summary");
        assert!(
            report < summary,
            "the report comes before the summary\n{out:#?}"
        );
        assert!(
            out.stderr.contains("    error[runtime]: task <handle:"),
            "the report is indented\n{out:#?}"
        );
    }
}

/// A task spawned by a task of a test belongs to that test too.
#[test]
fn test_failure_of_a_nested_task_fails_the_test_that_spawned_its_parent() {
    let src = r#"import channel
import task
import test
import time

fn test_nested() {
  let about_to_fail = channel.new(1)
  let outer = task.spawn(fn() {
    let _ = task.spawn(fn() {
      channel.send(about_to_fail, 1)
      let zero = 0
      10 / zero
    })
    42
  })
  test.assert_eq(task.join(outer), 42)
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
}

fn test_other() {
  test.assert_eq(2, 2)
}
"#;
    for out in run_tests("test_nested", src) {
        assert_eq!(out.code, Some(1), "{out:#?}");
        assert!(
            line_starting_with(&out.stderr, "FAIL tasks_test.silt::test_nested").is_some(),
            "the test whose task spawned the failing task fails\n{out:#?}"
        );
        assert!(out.stderr.contains(NEVER_JOINED), "{out:#?}");
        assert!(
            out.stderr
                .contains("2 tests: 1 passed, 1 failed, 0 skipped"),
            "{out:#?}"
        );
    }
}

/// A test that joins or cancels its failed tasks passes. Passes with
/// and without the fix.
#[test]
fn guard_test_that_cancels_its_failed_task_passes() {
    let src = r#"import channel
import task
import test
import time

fn test_cancels() {
  let about_to_fail = channel.new(1)
  let h = task.spawn(fn() {
    channel.send(about_to_fail, 1)
    let zero = 0
    10 / zero
  })
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  task.cancel(h)
  test.assert_eq(1, 1)
}
"#;
    // Without the fix the cancelled failure is still reported, but the
    // test passes and the run exits 0 either way; the report is what
    // `cancel_after_failure_dismisses_the_failure` pins.
    for out in run_tests("guard_test_cancels", src) {
        assert_eq!(out.code, Some(0), "{out:#?}");
        assert!(
            out.stderr.contains("PASS tasks_test.silt::test_cancels"),
            "{out:#?}"
        );
        assert!(
            out.stderr.contains("1 test: 1 passed, 0 failed, 0 skipped"),
            "{out:#?}"
        );
    }
}

// ── REPL ─────────────────────────────────────────────────────────────

/// The REPL reports a task failure after the input during which it
/// happened, not when the session ends. The last input is a name that
/// does not exist, whose type error marks the end of the session on
/// stderr; the report must come before it.
#[test]
fn repl_reports_task_failure_after_the_input() {
    let input = "import task\n\
                 import time\n\
                 let h = task.spawn(fn() { 1 / 0 })\n\
                 time.sleep(time.ms(300))\n\
                 marker_after_the_failure\n\
                 :quit\n";
    for run in 1..=REPEATS {
        let out = run_silt("repl", "unused.silt", "", &["repl"], Some(input));
        assert!(!out.timed_out, "run {run}: the REPL hung\n{out:#?}");
        let report = out
            .stderr
            .find(NEVER_JOINED)
            .unwrap_or_else(|| panic!("run {run}: the failure must be reported\n{out:#?}"));
        let marker = out
            .stderr
            .find("marker_after_the_failure")
            .unwrap_or_else(|| panic!("run {run}: the marker input must be answered\n{out:#?}"));
        assert!(
            report < marker,
            "run {run}: the failure must be reported after the input during which it \
             happened, before later inputs\n{out:#?}"
        );
        assert_eq!(
            out.stderr.matches(REPORT_HEADER).count(),
            1,
            "run {run}: one failure, one report\n{out:#?}"
        );
        assert!(
            !out.stderr.contains("<input>"),
            "run {run}: the REPL shows no `<input>` locator\n{out:#?}"
        );
    }
}
