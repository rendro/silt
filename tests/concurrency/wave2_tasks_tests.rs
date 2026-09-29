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
//!   * The REPL reports such failures when the session ends, rendered
//!     without `<input>`; a task that a later input joins is not reported.
//!
//! The `silt run` and `silt test` scenarios are golden cases:
//! `tests/golden/concurrency/unjoined/wave2_tasks__*`. The REPL
//! scenarios stay here: they drive `silt repl` over stdin.
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

// The fields are shown in failure messages through `Debug`.
#[allow(dead_code)]
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

// ── REPL ─────────────────────────────────────────────────────────────

/// The REPL reports a failed task that nobody joined once, when the
/// session ends: after the last input's output, not before it.
#[test]
fn repl_reports_an_unjoined_task_failure_when_the_session_ends() {
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
            marker < report,
            "run {run}: the failure is reported when the session ends\n{out:#?}"
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

/// A task that fails and is joined by a later input is not reported as
/// unjoined: the join raises its error, and that is all.
#[test]
fn repl_task_joined_by_a_later_input_is_not_reported() {
    let input = "import task\n\
                 import time\n\
                 let h = task.spawn(fn() { 1 / 0 })\n\
                 time.sleep(time.ms(300))\n\
                 task.join(h)\n\
                 :quit\n";
    for run in 1..=REPEATS {
        let out = run_silt("repl", "unused.silt", "", &["repl"], Some(input));
        assert!(!out.timed_out, "run {run}: the REPL hung\n{out:#?}");
        assert!(
            out.stderr.contains("joined task failed: division by zero"),
            "run {run}: the join raises the task's error\n{out:#?}"
        );
        assert!(
            !out.stderr.contains(NEVER_JOINED),
            "run {run}: a joined task is not reported as unjoined\n{out:#?}"
        );
    }
}
