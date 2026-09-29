//! Audit regression: the `--watch` interceptor must respect the `--`
//! program-args separator.
//!
//! Pre-fix (`src/cli/watch.rs`): `maybe_handle_watch` scanned the ENTIRE
//! arg vector for `--watch` / `-w` with no awareness of `--`. So
//! `silt run prog.silt -- -w` (where `-w` is the PROGRAM's own flag,
//! forwarded via `io.args()`) matched, watch mode was entered, and
//! `handle_watch` then filtered out EVERY `-w` — re-invoking the
//! subprocess as `silt run prog.silt --` with empty program args. Net
//! effect: the program (a) never saw its `-w` and (b) was silently run
//! under the file-watch loop instead of executing exactly once.
//!
//! Post-fix: only the CLI-flag region BEFORE the first standalone `--`
//! is scanned/filtered; the post-`--` tail is forwarded verbatim. So
//! `silt run prog.silt -- -w` runs the program exactly once and
//! `io.args()` returns `["-w"]`.
//!
//! Each test shells out to the real `silt` binary so the full argv
//! parsing + watch-interception path is exercised end-to-end. Because a
//! correct (post-fix) run executes ONCE and exits, `.output()` returns
//! promptly. A regressed build would enter the watch loop and `.output()`
//! would block forever, so each invocation is run on a worker thread with
//! a bounded join timeout — a timeout is treated as a test failure (the
//! process is still watching).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn fresh_dir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("silt_watch_dd_{pid}_{prefix}_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a script that prints each `io.args()` entry on its own line.
fn write_args_dumper(path: &std::path::Path) {
    fs::write(
        path,
        "import io\n\
         import list\n\
         fn main() {\n  \
           io.args() |> list.each { arg -> println(arg) }\n\
         }\n",
    )
    .unwrap();
}

/// Run `silt <args...>` with a hard wall-clock cap. Returns the captured
/// (stdout, success) on completion. Panics if the process does not exit
/// within `timeout` — which is exactly the regressed behavior (entered
/// the watch loop and is sitting there forever instead of running once).
fn run_with_timeout(args: Vec<String>, timeout: Duration) -> (String, bool) {
    let (tx, rx) = mpsc::channel();
    let label = args.join(" ");
    thread::spawn(move || {
        let out = Command::new(env!("CARGO_BIN_EXE_silt"))
            .args(&args)
            .output()
            .expect("failed to spawn silt");
        let _ = tx.send((
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.status.success(),
        ));
    });
    match rx.recv_timeout(timeout) {
        Ok(res) => res,
        Err(_) => panic!(
            "`silt {label}` did not exit within {timeout:?} — it entered the watch loop \
             instead of running the program exactly once (the `--watch` interceptor \
             hijacked a post-`--` program arg)"
        ),
    }
}

#[test]
fn run_dash_w_after_separator_runs_once_and_forwards_arg() {
    let dir = fresh_dir("run_w");
    let script = dir.join("dump_args.silt");
    write_args_dumper(&script);

    let (stdout, success) = run_with_timeout(
        vec![
            "run".into(),
            script.to_string_lossy().into_owned(),
            "--".into(),
            "-w".into(),
        ],
        Duration::from_secs(15),
    );

    assert!(
        success,
        "`silt run <file> -- -w` must run the program once and exit 0; got stdout: {stdout}"
    );
    // The `-w` must reach the program via io.args(), not be consumed as a
    // silt CLI flag.
    assert!(
        stdout.lines().any(|l| l.trim() == "-w"),
        "program must observe '-w' in io.args(); got stdout: {stdout}"
    );
}

#[test]
fn run_long_watch_after_separator_runs_once_and_forwards_arg() {
    let dir = fresh_dir("run_watch");
    let script = dir.join("dump_args.silt");
    write_args_dumper(&script);

    let (stdout, success) = run_with_timeout(
        vec![
            "run".into(),
            script.to_string_lossy().into_owned(),
            "--".into(),
            "--watch".into(),
        ],
        Duration::from_secs(15),
    );

    assert!(
        success,
        "`silt run <file> -- --watch` must run the program once and exit 0; got stdout: {stdout}"
    );
    assert!(
        stdout.lines().any(|l| l.trim() == "--watch"),
        "program must observe '--watch' in io.args(); got stdout: {stdout}"
    );
}

#[test]
fn check_dash_w_after_separator_does_one_shot_check() {
    // `silt check <file> -- -w` must one-shot check (exit promptly), not
    // enter the watch loop. `check` doesn't execute, so we only assert it
    // returns within the timeout and succeeds on a valid program.
    let dir = fresh_dir("check_w");
    let script = dir.join("ok.silt");
    fs::write(&script, "fn main() { print(\"hi\") }\n").unwrap();

    let (_stdout, success) = run_with_timeout(
        vec![
            "check".into(),
            script.to_string_lossy().into_owned(),
            "--".into(),
            "-w".into(),
        ],
        Duration::from_secs(15),
    );

    assert!(
        success,
        "`silt check <file> -- -w` must one-shot check and exit 0, not enter watch mode"
    );
}
