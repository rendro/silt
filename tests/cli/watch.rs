//! End-to-end integration tests for `silt run -w` (watch mode).
//!
//! These tests drive `watch_and_rerun` through the real binary: they spawn
//! `silt run -w <file.silt>`, capture stdout in a background reader thread,
//! mutate files on disk, and assert that the subprocess reruns (or does not)
//! as expected. The unit tests in `src/watch.rs` drive the watch loop with
//! scripted events and a scripted command; these tests cover the real OS
//! watcher and child process.
//!
//! An edit is made as soon as the previous run's output is seen: the
//! watcher compares file contents with what they were when the command
//! started, so no edit has to wait for a window to pass. Output is
//! collected by polling a shared buffer with a bounded total timeout, so
//! tests do not hang if watch mode misbehaves.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A freshly created per-test temp directory under the system temp dir.
/// Deleted on drop so tests clean up even if they panic.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("silt_watch_tests_{pid}_{prefix}_{n}"));
        // If a stale directory from a previous run with the same pid/name
        // exists, clear it so tests start from a clean slate.
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// RAII handle around a spawned `silt -w` subprocess that collects its
/// stdout into a shared buffer on a background thread. Kills the child on
/// drop so tests never leak processes, even on panic.
struct WatchProc {
    child: Child,
    stdout: Arc<Mutex<Vec<u8>>>,
}

impl WatchProc {
    /// Spawn `silt run -w <file>` with piped stdout and start a reader
    /// thread that drains stdout into `self.stdout` until EOF.
    fn spawn(file: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("run")
            .arg("-w")
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn silt -w");

        let stdout = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut child_stdout = child.stdout.take().expect("piped stdout");
        let stdout_clone = Arc::clone(&stdout);
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match child_stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut guard = stdout_clone.lock().unwrap();
                        guard.extend_from_slice(&buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        });

        WatchProc { child, stdout }
    }

    /// Snapshot of stdout collected so far.
    fn stdout_snapshot(&self) -> String {
        let guard = self.stdout.lock().unwrap();
        String::from_utf8_lossy(&guard).into_owned()
    }

    /// Poll `stdout_snapshot` until `pred(snapshot)` returns true or
    /// `timeout` elapses. Returns the matching snapshot, or panics with a
    /// helpful message on timeout.
    fn wait_until<F>(&self, label: &str, timeout: Duration, mut pred: F) -> String
    where
        F: FnMut(&str) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            let snap = self.stdout_snapshot();
            if pred(&snap) {
                return snap;
            }
            if Instant::now() >= deadline {
                panic!(
                    "watch test timed out waiting for: {label}\n\
                     ── stdout so far ──\n{snap}\n──────────────────"
                );
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for WatchProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Count how many times `needle` appears in `haystack`.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while let Some(idx) = haystack[start..].find(needle) {
        count += 1;
        start += idx + needle.len();
    }
    count
}

/// A tiny "hello world"-style silt program that prints `marker` so tests
/// can key off a unique string per test run.
fn program_printing(marker: &str) -> String {
    format!("fn main() {{\n  println(\"{marker}\")\n}}\n")
}

// ── 1. Initial run executes the file and prints its output ─────────

#[test]
fn watch_initial_run_prints_output() {
    let dir = TempDir::new("initial");
    let file = dir.path().join("main.silt");
    fs::write(&file, program_printing("first-run-marker-abc")).unwrap();

    let proc = WatchProc::spawn(&file);

    proc.wait_until(
        "initial run output 'first-run-marker-abc'",
        Duration::from_secs(10),
        |snap| snap.contains("first-run-marker-abc"),
    );
}

// ── 2. Modifying the watched file triggers a rerun ─────────────────

#[test]
fn watch_rerun_on_modification() {
    let dir = TempDir::new("rerun");
    let file = dir.path().join("main.silt");
    fs::write(&file, program_printing("before-edit-xyz")).unwrap();

    let proc = WatchProc::spawn(&file);

    // Wait for initial run to print its marker.
    proc.wait_until(
        "initial 'before-edit-xyz'",
        Duration::from_secs(10),
        |snap| snap.contains("before-edit-xyz"),
    );

    // Modify the file with new content.
    fs::write(&file, program_printing("after-edit-xyz")).unwrap();

    // Wait for the post-edit marker to appear.
    let snap = proc.wait_until("rerun 'after-edit-xyz'", Duration::from_secs(10), |snap| {
        snap.contains("after-edit-xyz")
    });

    // Sanity: the first marker is still present (watch mode didn't wipe
    // the stdout pipe) and the new marker really did come from a rerun,
    // not from the initial output.
    assert!(
        snap.contains("before-edit-xyz"),
        "original output should still be present in stdout history:\n{snap}"
    );
}

// ── 3. Creating an UNRELATED (non-.silt) file does not trigger a rerun ─

#[test]
fn watch_ignores_non_silt_changes() {
    let dir = TempDir::new("ignore");
    let file = dir.path().join("main.silt");
    fs::write(&file, program_printing("only-once-qqq")).unwrap();

    let proc = WatchProc::spawn(&file);

    // Wait for initial run.
    proc.wait_until("initial 'only-once-qqq'", Duration::from_secs(10), |snap| {
        snap.contains("only-once-qqq")
    });

    let initial_count = count_occurrences(&proc.stdout_snapshot(), "only-once-qqq");
    assert_eq!(
        initial_count, 1,
        "expected exactly one initial print, got {initial_count}"
    );

    // Create and then modify a file of the same directory that is not a
    // file of the program. Its events are ignored.
    let txt = dir.path().join("notes.txt");
    fs::write(&txt, "some notes").unwrap();
    thread::sleep(Duration::from_millis(200));
    fs::write(&txt, "updated notes").unwrap();

    // Wait a generous window to give the watcher a chance to misbehave.
    // If watch mode were wrongly triggering on .txt changes, the program
    // would rerun and we'd see `only-once-qqq` a second time.
    thread::sleep(Duration::from_millis(1500));

    let final_count = count_occurrences(&proc.stdout_snapshot(), "only-once-qqq");
    assert_eq!(
        final_count,
        1,
        "non-.silt file change must not trigger a rerun (saw {final_count} prints):\n{}",
        proc.stdout_snapshot()
    );
}

// ── 4. A syntax error does not crash watch mode; next valid edit recovers ─

#[test]
fn watch_recovers_from_syntax_error() {
    let dir = TempDir::new("recover");
    let file = dir.path().join("main.silt");
    fs::write(&file, program_printing("good-v1-mno")).unwrap();

    let proc = WatchProc::spawn(&file);

    // Wait for initial run to succeed.
    proc.wait_until("initial 'good-v1-mno'", Duration::from_secs(10), |snap| {
        snap.contains("good-v1-mno")
    });

    // Edit to a syntactically invalid program. Watch mode should print a
    // parse error (to stderr, which we're discarding) but keep running.
    fs::write(&file, "fn { this is not valid silt !!!").unwrap();

    // Give watch mode time to observe the broken file and attempt to run it.
    // We can't easily assert on the error message (stderr is /dev/null), but
    // we *can* verify the subprocess is still alive by issuing another valid
    // edit and observing that it is picked up.
    thread::sleep(Duration::from_millis(800));

    // Now write a valid program again with a fresh marker.
    fs::write(&file, program_printing("good-v2-pqr")).unwrap();

    // The recovered valid file should be executed — its marker should show up.
    proc.wait_until(
        "recovered 'good-v2-pqr' after syntax error",
        Duration::from_secs(10),
        |snap| snap.contains("good-v2-pqr"),
    );
}

// ── 5. A change reloads a program that never ends ─────────────────

/// A program that prints `marker` and then never ends.
fn endless_program_printing(marker: &str) -> String {
    format!(
        "import time\n\nfn spin() {{\n  time.sleep(time.ms(50))\n  spin()\n}}\n\nfn main() {{\n  println(\"{marker}\")\n  spin()\n}}\n"
    )
}

#[test]
fn watch_kills_a_running_program_on_change() {
    let dir = TempDir::new("kill");
    let file = dir.path().join("main.silt");
    fs::write(&file, endless_program_printing("endless-v1")).unwrap();

    let proc = WatchProc::spawn(&file);
    proc.wait_until("first 'endless-v1'", Duration::from_secs(10), |snap| {
        snap.contains("endless-v1")
    });

    // The first run never ends: only killing it lets the edit run.
    fs::write(&file, endless_program_printing("endless-v2")).unwrap();
    proc.wait_until(
        "'endless-v2' after the running program was killed",
        Duration::from_secs(10),
        |snap| snap.contains("endless-v2"),
    );
}

// ── 6. A change to an imported module, in another directory, reruns ─

#[test]
fn watch_reruns_on_a_change_to_an_imported_module() {
    let dir = TempDir::new("import");
    fs::write(
        dir.path().join("silt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    let main = src.join("main.silt");
    fs::write(
        &main,
        "import util\n\nfn main() {\n  println(util.marker())\n}\n",
    )
    .unwrap();
    let util = src.join("util.silt");
    fs::write(&util, "pub fn marker() {\n  \"util-v1\"\n}\n").unwrap();

    let proc = WatchProc::spawn(&main);
    proc.wait_until("first 'util-v1'", Duration::from_secs(10), |snap| {
        snap.contains("util-v1")
    });

    fs::write(&util, "pub fn marker() {\n  \"util-v2\"\n}\n").unwrap();
    proc.wait_until("rerun 'util-v2'", Duration::from_secs(10), |snap| {
        snap.contains("util-v2")
    });
}

// ── 7. count_occurrences sanity check ──────────────────────────────

#[test]
fn count_occurrences_helper_is_correct() {
    assert_eq!(count_occurrences("", "x"), 0);
    assert_eq!(count_occurrences("xxxx", ""), 0);
    assert_eq!(count_occurrences("abcabcabc", "abc"), 3);
    assert_eq!(count_occurrences("aaaa", "aa"), 2); // non-overlapping
    assert_eq!(count_occurrences("hello world", "o"), 2);
}
