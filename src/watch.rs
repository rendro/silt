//! Watch mode: run a silt command, and run it again whenever a file of
//! the program changes.
//!
//! The command runs as a child process (the same `silt` binary without
//! `--watch`). The files watched are those of the program: every module
//! file the session's analysis of each entry reaches (an import that
//! names a missing file included, so creating it is a change), plus the
//! project's `silt.toml` and `silt.lock`. A change kills the child if it
//! is still running, then runs the command again, so a program that
//! never ends (a server) is reloaded too.
//!
//! A change is a change of content: an event for a watched file whose
//! text is what it was when the command last started is ignored, and so
//! is an event for any other file.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::manifest::Manifest;
use crate::session::{Config, LockPolicy, ProjectSetup, Session};

/// Banner printed when the command has finished, so the user knows the
/// watcher is armed for the next save. Test suites grep for
/// `[watch] Watching for changes` (substring) to detect the watcher's
/// signature output, so any future tweak should preserve that substring.
const WATCH_BANNER: &str = "\n[watch] Watching for changes...";

/// How long the watcher waits after a change for more of them (an editor
/// writes a file in several steps), so a save runs the command once.
const SETTLE: Duration = Duration::from_millis(100);

/// How often the watcher looks whether a running command has finished.
const POLL: Duration = Duration::from_millis(50);

/// Clear-screen + cursor-home escape sequence, gated on stderr being a
/// real terminal. When stderr is redirected (e.g. `silt run app.silt
/// --watch 2> watch.log`), this returns `""` so the literal escape bytes
/// (`^[[2J^[[H`) are never written into the log. Honors `NO_COLOR` for
/// parity with the rest of silt's terminal-control gating (see
/// `diagnostic::use_color`'s `is_terminal`-based color decision).
fn clear_screen_seq() -> &'static str {
    if std::env::var_os("NO_COLOR").is_some() {
        return "";
    }
    if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        "\x1B[2J\x1B[H"
    } else {
        ""
    }
}

/// The files of a program and what each held: a hash of its text, or
/// `None` for a file that cannot be read (a missing module).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WatchSet {
    files: BTreeMap<PathBuf, Option<u64>>,
}

impl WatchSet {
    /// The files `paths`, each with what it holds now.
    pub fn of(paths: impl IntoIterator<Item = PathBuf>) -> WatchSet {
        WatchSet {
            files: paths
                .into_iter()
                .map(|path| {
                    let content = content_hash(&path);
                    (path, content)
                })
                .collect(),
        }
    }

    /// The files of the programs that start at `entries`: the files of
    /// each one's analysis, and its project's `silt.toml` and
    /// `silt.lock`.
    pub fn for_entries(entries: &[PathBuf]) -> WatchSet {
        let mut paths = BTreeSet::new();
        for entry in entries {
            let entry = absolute(entry);
            let dir = entry
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let mut session = Session::new(Config {
                project: ProjectSetup::Discover(dir.clone()),
                lock: LockPolicy::ReadOnly,
                host: Vec::new(),
            });
            match session.open(&entry) {
                Ok(file) => {
                    session.analyze(file);
                    paths.extend(session.files(file).into_iter().map(|p| absolute(&p)));
                }
                Err(_) => {
                    paths.insert(entry.clone());
                }
            }
            if let Some(root) = Manifest::find(&dir) {
                paths.insert(root.join("silt.toml"));
                paths.insert(root.join("silt.lock"));
            }
        }
        WatchSet::of(paths)
    }

    /// The files of the set.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.files.keys().map(PathBuf::as_path)
    }

    /// Whether `path` is a file of the set.
    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    /// Whether a file of the set holds something else now than it did.
    pub fn changed(&self) -> bool {
        self.files
            .iter()
            .any(|(path, content)| content_hash(path) != *content)
    }

    /// The directories the files are in: what to watch, so a file that
    /// an editor replaces (or that does not exist yet) is seen.
    fn dirs(&self) -> BTreeSet<PathBuf> {
        self.files
            .keys()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect()
    }
}

/// A hash of the text of the file at `path`, or `None` when it cannot be
/// read.
fn content_hash(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

/// `path` made absolute against the working directory, so it compares
/// equal to the paths file events carry.
fn absolute(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    path.canonicalize().unwrap_or(path)
}

/// The command a watcher runs and stops.
pub trait Runner {
    /// Start the command.
    fn start(&mut self);
    /// Whether the command started last has finished.
    fn finished(&mut self) -> bool;
    /// Stop the command if it is still running, and wait for it.
    fn stop(&mut self);
}

/// The `silt` binary run with `args`, as a child process.
struct ChildRunner {
    exe: PathBuf,
    args: Vec<String>,
    child: Option<Child>,
}

impl Runner for ChildRunner {
    fn start(&mut self) {
        eprint!("{}", clear_screen_seq());
        match Command::new(&self.exe).args(&self.args).spawn() {
            Ok(child) => self.child = Some(child),
            Err(e) => eprintln!("error: failed to run {}: {e}", self.exe.display()),
        }
    }

    fn finished(&mut self) -> bool {
        match &mut self.child {
            Some(child) => !matches!(child.try_wait(), Ok(None)),
            None => true,
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Run `silt <args>` and run it again on every change to the files of
/// the programs that start at `entries()`. `entries` is asked again
/// before each run, so a new test file is found.
pub fn watch_and_rerun(entries: impl Fn() -> Vec<PathBuf>, args: &[String]) {
    let (tx, rx) = mpsc::channel();
    // Creating the OS watcher can fail in restrictive environments: sandboxes
    // where inotify is disabled, read-only filesystems, containers that cap
    // file descriptors, or platforms where `notify`'s backend can't initialize.
    // Surface a helpful hint so users know they can fall back to a one-shot
    // compile instead of staring at a raw errno.
    let watcher_failed = |e: notify::Error| -> ! {
        eprintln!(
            "error: failed to start file watcher: {e}. Try running without --watch to compile once."
        );
        std::process::exit(1);
    };
    let mut watcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(event) => {
                let _ = tx.send(event.paths);
            }
            Err(e) => eprintln!("watch error: {e}"),
        })
        .unwrap_or_else(|e| watcher_failed(e));

    let exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!("error: failed to get executable path: {e}");
        std::process::exit(1);
    });
    let mut runner = ChildRunner {
        exe,
        args: args.to_vec(),
        child: None,
    };
    let mut watched: BTreeSet<PathBuf> = BTreeSet::new();
    run(
        &rx,
        || WatchSet::for_entries(&entries()),
        |set| {
            let dirs = set.dirs();
            for dir in watched.difference(&dirs) {
                let _ = watcher.unwatch(dir);
            }
            for dir in dirs.difference(&watched) {
                if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
                    // The first directory is the entry's: without it
                    // there is nothing to watch.
                    if watched.is_empty() {
                        watcher_failed(e);
                    }
                }
            }
            watched = dirs;
        },
        &mut runner,
        SETTLE,
    );
}

/// The watch loop. Before each run it takes the program's files
/// (`files`), has them watched (`watch`), and starts the command. Each
/// item of `events` is the paths of one file event. An event for a file
/// of the set, once `settle` has passed with no further event, is a
/// change if the set's content changed: the command is stopped and run
/// again. When `events` is closed, the command is stopped and the loop
/// returns.
pub fn run(
    events: &Receiver<Vec<PathBuf>>,
    mut files: impl FnMut() -> WatchSet,
    mut watch: impl FnMut(&WatchSet),
    runner: &mut impl Runner,
    settle: Duration,
) {
    loop {
        // The files are read before the command starts: a save made
        // while it runs is a change.
        let set = files();
        watch(&set);
        runner.start();
        let mut finished = false;
        loop {
            let paths = if finished {
                match events.recv() {
                    Ok(paths) => paths,
                    Err(_) => return,
                }
            } else {
                match events.recv_timeout(POLL) {
                    Ok(paths) => paths,
                    Err(RecvTimeoutError::Timeout) => {
                        if runner.finished() {
                            finished = true;
                            eprintln!("{WATCH_BANNER}");
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        runner.stop();
                        return;
                    }
                }
            };
            if !paths.iter().any(|p| set.contains(&absolute(p))) {
                continue;
            }
            // Let the writes settle, then take what arrived meanwhile.
            if !settle.is_zero() {
                std::thread::sleep(settle);
            }
            while events.try_recv().is_ok() {}
            if set.changed() {
                runner.stop();
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;

    // ── clear_screen_seq TTY guard ────────────────────────────────
    //
    // The clear-screen escape must not be written when stderr is
    // redirected. Under `cargo test` stderr is captured (not a terminal),
    // so the helper must return "".
    #[test]
    fn clear_screen_seq_empty_when_not_terminal() {
        assert_eq!(clear_screen_seq(), "");
    }

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A fresh directory for one test.
    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("silt_watch_unit_{}_{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    /// A runner that records what the loop does with it and, when the
    /// `n`th run starts, does what `on_start(n)` says: edits files and
    /// sends the events an OS watcher would, or closes the events.
    struct Script {
        log: Vec<String>,
        runs: usize,
        running: bool,
        on_start: Box<dyn FnMut(usize)>,
    }

    impl Runner for Script {
        fn start(&mut self) {
            self.runs += 1;
            self.running = true;
            self.log.push(format!("start {}", self.runs));
            (self.on_start)(self.runs);
        }
        fn finished(&mut self) -> bool {
            !self.running
        }
        fn stop(&mut self) {
            if self.running {
                self.log.push(format!("kill {}", self.runs));
            }
            self.running = false;
        }
    }

    /// Run the loop over the files `paths` with a script: the events
    /// channel is closed after the run `last` starts.
    fn drive(
        paths: Vec<PathBuf>,
        last: usize,
        mut step: impl FnMut(usize, &Sender<Vec<PathBuf>>) + 'static,
    ) -> Vec<String> {
        let (tx, rx) = mpsc::channel();
        let mut tx = Some(tx);
        let mut runner = Script {
            log: Vec::new(),
            runs: 0,
            running: false,
            on_start: Box::new(move |n| {
                if let Some(sender) = &tx {
                    step(n, sender);
                }
                if n >= last {
                    tx = None;
                }
            }),
        };
        run(
            &rx,
            || WatchSet::of(paths.clone()),
            |_| {},
            &mut runner,
            Duration::ZERO,
        );
        runner.log
    }

    #[test]
    fn a_change_while_the_command_runs_kills_it_before_the_rerun() {
        let dir = temp_dir();
        let main = dir.join("main.silt");
        std::fs::write(&main, "v1").unwrap();
        let edited = main.clone();
        let log = drive(vec![main], 2, move |n, tx| {
            if n == 1 {
                std::fs::write(&edited, "v2").unwrap();
                tx.send(vec![edited.clone()]).unwrap();
            }
        });
        assert_eq!(log, ["start 1", "kill 1", "start 2", "kill 2"]);
    }

    #[test]
    fn an_event_for_a_file_outside_the_set_is_ignored() {
        let dir = temp_dir();
        let main = dir.join("main.silt");
        let notes = dir.join("notes.silt");
        std::fs::write(&main, "v1").unwrap();
        let log = drive(vec![main], 1, move |_, tx| {
            std::fs::write(&notes, "x").unwrap();
            tx.send(vec![notes.clone()]).unwrap();
        });
        assert_eq!(log, ["start 1", "kill 1"]);
    }

    #[test]
    fn an_event_with_the_same_content_is_ignored() {
        let dir = temp_dir();
        let main = dir.join("main.silt");
        std::fs::write(&main, "v1").unwrap();
        let touched = main.clone();
        let log = drive(vec![main], 1, move |_, tx| {
            std::fs::write(&touched, "v1").unwrap();
            tx.send(vec![touched.clone()]).unwrap();
        });
        assert_eq!(log, ["start 1", "kill 1"]);
    }

    #[test]
    fn creating_a_missing_watched_file_is_a_change() {
        let dir = temp_dir();
        let util = dir.join("util.silt");
        let created = util.clone();
        let log = drive(vec![util], 2, move |n, tx| {
            if n == 1 {
                std::fs::write(&created, "pub fn f() { 1 }").unwrap();
                tx.send(vec![created.clone()]).unwrap();
            }
        });
        assert_eq!(log, ["start 1", "kill 1", "start 2", "kill 2"]);
    }

    #[test]
    fn a_finished_command_is_not_killed_on_change() {
        let dir = temp_dir();
        let main = dir.join("main.silt");
        std::fs::write(&main, "v1").unwrap();
        let (tx, rx) = mpsc::channel();
        let edited = main.clone();
        let mut runner = Script {
            log: Vec::new(),
            runs: 0,
            running: false,
            on_start: Box::new(|_| {}),
        };
        // The command finishes at once: the loop sees that, then the
        // edit, then the end of the events.
        let mut tx = Some(tx);
        let mut first = true;
        run(
            &rx,
            || WatchSet::of(vec![main.clone()]),
            |_| {},
            &mut FinishingRunner {
                inner: &mut runner,
                on_finished: Box::new(move || {
                    if first {
                        first = false;
                        std::fs::write(&edited, "v2").unwrap();
                        tx.as_ref().unwrap().send(vec![edited.clone()]).unwrap();
                    } else {
                        tx = None;
                    }
                }),
            },
            Duration::ZERO,
        );
        assert_eq!(runner.log, ["start 1", "start 2"]);
    }

    /// A runner whose command finishes as soon as it starts, and that
    /// calls `on_finished` when the loop sees that.
    struct FinishingRunner<'a> {
        inner: &'a mut Script,
        on_finished: Box<dyn FnMut()>,
    }

    impl Runner for FinishingRunner<'_> {
        fn start(&mut self) {
            self.inner.start();
            self.inner.running = false;
        }
        fn finished(&mut self) -> bool {
            (self.on_finished)();
            true
        }
        fn stop(&mut self) {
            self.inner.stop();
        }
    }

    #[test]
    fn the_set_is_the_files_of_the_analysis_and_the_project_files() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("silt.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("main.silt"),
            "import util\nimport missing\nfn main() { util.f() }\n",
        )
        .unwrap();
        std::fs::write(src.join("util.silt"), "pub fn f() { 1 }\n").unwrap();
        std::fs::write(src.join("other.silt"), "pub fn g() { 1 }\n").unwrap();
        let set = WatchSet::for_entries(&[src.join("main.silt")]);
        let paths: Vec<&Path> = set.paths().collect();
        assert_eq!(
            paths,
            [
                dir.join("silt.lock").as_path(),
                dir.join("silt.toml").as_path(),
                src.join("main.silt").as_path(),
                src.join("missing.silt").as_path(),
                src.join("util.silt").as_path(),
            ]
        );
    }
}
