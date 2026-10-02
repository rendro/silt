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
//! For `silt test` over a directory, the directory is watched with its
//! subdirectories too: a test file created or removed there is a change.
//!
//! A change is a change of content: an event for a watched file whose
//! text is what it was when the command last started is ignored, and so
//! is an event for any other file. The watcher resolves the packages
//! under the command's lockfile policy before it reads the files, so the
//! lockfile the command would rewrite is rewritten first and is not a
//! change.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::manifest::Manifest;
use crate::package_graph::LockChange;
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
/// `None` for a file that cannot be read (a missing module). For a
/// command whose entries are discovered in a directory (`silt test`), the
/// directory and the entries found there.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WatchSet {
    files: BTreeMap<PathBuf, Option<u64>>,
    discovery: Option<(PathBuf, BTreeSet<PathBuf>)>,
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
            discovery: None,
        }
    }

    /// The set, with `entries` discovered in the directory `dir`: a
    /// `.silt` file created or removed under it is looked at, and when
    /// the entries found there are no longer `entries`, that is a change.
    pub fn discovered_in(mut self, dir: PathBuf, entries: BTreeSet<PathBuf>) -> WatchSet {
        self.discovery = Some((dir, entries));
        self
    }

    /// The files of the programs that start at `entries`: the files of
    /// each one's analysis, and its project's `silt.toml` and
    /// `silt.lock`. Each program's packages are resolved under `lock`,
    /// the policy of the command watched: a lockfile the command would
    /// rewrite is rewritten here, before the files are read, so the
    /// command's own rewrite is not a change.
    pub fn for_entries(entries: &[PathBuf], lock: LockPolicy) -> WatchSet {
        let mut paths = BTreeSet::new();
        for entry in entries {
            let entry = absolute(entry);
            let dir = entry
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let mut session = Session::new(Config {
                project: ProjectSetup::Discover(dir.clone()),
                lock,
                host: Vec::new(),
            });
            if let Ok(packages) = session.packages()
                && packages.lock == LockChange::Updated
            {
                eprintln!("Updating silt.lock for new dependencies in silt.toml");
            }
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

    /// Whether an event for `path` is to be looked at: a file of the set,
    /// or a `.silt` file under the discovery directory.
    pub fn concerns(&self, path: &Path) -> bool {
        self.files.contains_key(path)
            || self.discovery.as_ref().is_some_and(|(dir, _)| {
                path.starts_with(dir) && path.extension().is_some_and(|ext| ext == "silt")
            })
    }

    /// Whether a file of the set holds something else now than it did,
    /// or the entries discovered now (`entries()`) are others.
    pub fn changed(&self, entries: impl FnOnce() -> BTreeSet<PathBuf>) -> bool {
        self.files
            .iter()
            .any(|(path, content)| content_hash(path) != *content)
            || self
                .discovery
                .as_ref()
                .is_some_and(|(_, found)| entries() != *found)
    }

    /// The directories to watch, and whether each is watched with its
    /// subdirectories: the directories the files are in (so a file that
    /// an editor replaces, or that does not exist yet, is seen), and the
    /// discovery directory with its subdirectories.
    fn watches(&self) -> BTreeSet<(PathBuf, bool)> {
        let discovery = self.discovery.as_ref().map(|(dir, _)| dir);
        let mut watches: BTreeSet<(PathBuf, bool)> = self
            .files
            .keys()
            .filter_map(|p| p.parent())
            .filter(|dir| !discovery.is_some_and(|d| dir.starts_with(d)))
            .map(|dir| (dir.to_path_buf(), false))
            .collect();
        if let Some(dir) = discovery {
            watches.insert((dir.clone(), true));
        }
        watches
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
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    // A file that does not exist (a missing module, a file just removed):
    // its directory may.
    match (path.parent().map(Path::canonicalize), path.file_name()) {
        (Some(Ok(dir)), Some(name)) => dir.join(name),
        _ => path,
    }
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

/// What a watcher watches: the programs of a command.
pub struct Target {
    /// The entry files of the programs, found again before each run.
    pub entries: Box<dyn Fn() -> Vec<PathBuf>>,
    /// The directory the entries are discovered in, for `silt test`
    /// given a directory or none: a test file created or removed there
    /// is a change.
    pub discovery: Option<PathBuf>,
    /// The lockfile policy of the command.
    pub lock: LockPolicy,
}

impl Target {
    /// The entries, as absolute paths.
    fn entry_set(&self) -> BTreeSet<PathBuf> {
        (self.entries)().iter().map(|p| absolute(p)).collect()
    }

    /// The files of the programs now.
    fn watch_set(&self) -> WatchSet {
        let entries = self.entry_set();
        let set = WatchSet::for_entries(&entries.iter().cloned().collect::<Vec<_>>(), self.lock);
        match &self.discovery {
            Some(dir) => set.discovered_in(absolute(dir), entries),
            None => set,
        }
    }
}

/// Run `silt <args>` and run it again on every change to the files of
/// the programs of `target`.
pub fn watch_and_rerun(target: Target, args: &[String]) {
    let (tx, rx) = mpsc::channel();
    // Creating the OS watcher can fail in restrictive environments: sandboxes
    // where inotify is disabled, read-only filesystems, containers that cap
    // file descriptors, or platforms where `notify`'s backend can't initialize.
    // Surface a helpful hint so users know they can fall back to a one-shot
    // compile instead of staring at a raw errno.
    let watcher_failed = |e: &dyn std::fmt::Display| -> ! {
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
        .unwrap_or_else(|e| watcher_failed(&e));

    let exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!("error: failed to get executable path: {e}");
        std::process::exit(1);
    });
    let mut runner = ChildRunner {
        exe,
        args: args.to_vec(),
        child: None,
    };
    let mut watched: BTreeSet<(PathBuf, bool)> = BTreeSet::new();
    let mut first = true;
    run(
        &rx,
        || target.watch_set(),
        || target.entry_set(),
        |set| {
            let watches = set.watches();
            for (dir, _) in watched.difference(&watches) {
                let _ = watcher.unwatch(dir);
            }
            let mut watching = watched.intersection(&watches).count();
            let mut last_error = None;
            for (dir, recursive) in watches.difference(&watched) {
                let mode = if *recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                };
                match watcher.watch(dir, mode) {
                    Ok(()) => watching += 1,
                    Err(e) => last_error = Some(e),
                }
            }
            // A directory that does not exist (that of a missing module)
            // cannot be watched, but with nothing watched at all the
            // watcher would never wake.
            if first && watching == 0 {
                match last_error {
                    Some(e) => watcher_failed(&e),
                    None => watcher_failed(&"there is no file to watch"),
                }
            }
            first = false;
            watched = watches;
        },
        &mut runner,
        SETTLE,
    );
}

/// The watch loop. Before each run it takes the program's files
/// (`files`), has them watched (`watch`), and starts the command. Each
/// item of `events` is the paths of one file event. An event the set
/// concerns, once `settle` has passed with no further event, is a change
/// if the set changed (`entries` gives the entries discovered now): the
/// command is stopped and run again. When `events` is closed, the
/// command is stopped and the loop returns.
pub fn run(
    events: &Receiver<Vec<PathBuf>>,
    mut files: impl FnMut() -> WatchSet,
    mut entries: impl FnMut() -> BTreeSet<PathBuf>,
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
            if !paths.iter().any(|p| set.concerns(&absolute(p))) {
                continue;
            }
            // Let the writes settle, then take what arrived meanwhile.
            if !settle.is_zero() {
                std::thread::sleep(settle);
            }
            while events.try_recv().is_ok() {}
            if set.changed(&mut entries) {
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
        step: impl FnMut(usize, &Sender<Vec<PathBuf>>) + 'static,
    ) -> Vec<String> {
        drive_with(
            move || WatchSet::of(paths.clone()),
            BTreeSet::new,
            last,
            step,
        )
    }

    /// [`drive`] with the set taken by `files` and the entries
    /// discovered now given by `entries`.
    fn drive_with(
        files: impl FnMut() -> WatchSet,
        entries: impl FnMut() -> BTreeSet<PathBuf>,
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
        run(&rx, files, entries, |_| {}, &mut runner, Duration::ZERO);
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
            BTreeSet::new,
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
        let set = WatchSet::for_entries(&[src.join("main.silt")], LockPolicy::ReadOnly);
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

    /// The test files under `dir`, as `silt test` finds them.
    fn test_files(dir: &Path) -> BTreeSet<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.to_string_lossy().ends_with("_test.silt"))
            .collect()
    }

    /// The set of a `silt test` over `dir`: its test files, discovered.
    fn test_set(dir: &Path) -> WatchSet {
        let entries = test_files(dir);
        WatchSet::of(entries.clone()).discovered_in(dir.to_path_buf(), entries)
    }

    #[test]
    fn a_new_test_file_is_a_change() {
        let dir = temp_dir();
        std::fs::write(dir.join("a_test.silt"), "fn test_a() { () }").unwrap();
        let (set_dir, entries_dir, step_dir) = (dir.clone(), dir.clone(), dir.clone());
        let log = drive_with(
            move || test_set(&set_dir),
            move || test_files(&entries_dir),
            2,
            move |n, tx| {
                if n == 1 {
                    let b = step_dir.join("b_test.silt");
                    std::fs::write(&b, "fn test_b() { () }").unwrap();
                    tx.send(vec![b]).unwrap();
                }
            },
        );
        assert_eq!(log, ["start 1", "kill 1", "start 2", "kill 2"]);
    }

    #[test]
    fn a_removed_test_file_is_a_change() {
        let dir = temp_dir();
        std::fs::write(dir.join("a_test.silt"), "fn test_a() { () }").unwrap();
        std::fs::write(dir.join("b_test.silt"), "fn test_b() { () }").unwrap();
        let (set_dir, entries_dir, step_dir) = (dir.clone(), dir.clone(), dir.clone());
        let log = drive_with(
            move || test_set(&set_dir),
            move || test_files(&entries_dir),
            2,
            move |n, tx| {
                if n == 1 {
                    let b = step_dir.join("b_test.silt");
                    std::fs::remove_file(&b).unwrap();
                    tx.send(vec![b]).unwrap();
                }
            },
        );
        assert_eq!(log, ["start 1", "kill 1", "start 2", "kill 2"]);
    }

    #[test]
    fn a_first_test_file_in_an_empty_directory_is_a_change() {
        let dir = temp_dir();
        let (set_dir, entries_dir, step_dir) = (dir.clone(), dir.clone(), dir.clone());
        let log = drive_with(
            move || test_set(&set_dir),
            move || test_files(&entries_dir),
            2,
            move |n, tx| {
                if n == 1 {
                    let a = step_dir.join("a_test.silt");
                    std::fs::write(&a, "fn test_a() { () }").unwrap();
                    tx.send(vec![a]).unwrap();
                }
            },
        );
        assert_eq!(log, ["start 1", "kill 1", "start 2", "kill 2"]);
        // And the empty directory is watched, so the event comes.
        let watches = test_set(&temp_dir()).watches();
        assert_eq!(watches.len(), 1);
        assert!(watches.iter().all(|(_, recursive)| *recursive));
    }

    #[test]
    fn a_new_silt_file_that_is_not_a_test_is_not_a_change() {
        let dir = temp_dir();
        std::fs::write(dir.join("a_test.silt"), "fn test_a() { () }").unwrap();
        let (set_dir, entries_dir, step_dir) = (dir.clone(), dir.clone(), dir.clone());
        let log = drive_with(
            move || test_set(&set_dir),
            move || test_files(&entries_dir),
            1,
            move |_, tx| {
                let helper = step_dir.join("helper.silt");
                std::fs::write(&helper, "pub fn h() { 1 }").unwrap();
                tx.send(vec![helper]).unwrap();
            },
        );
        assert_eq!(log, ["start 1", "kill 1"]);
    }

    #[test]
    fn the_lockfile_is_brought_up_to_date_before_the_files_are_read() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("silt.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.silt"), "fn main() { 1 }\n").unwrap();
        let lock = dir.join("silt.lock");
        assert!(!lock.exists());
        // Under the policy of `run`, the watcher's analysis writes the
        // lock the command would write, then reads the files: the
        // command finds the lock up to date and the set unchanged.
        let set = WatchSet::for_entries(&[src.join("main.silt")], LockPolicy::Update);
        assert!(lock.exists(), "the watcher's analysis writes silt.lock");
        assert!(set.paths().any(|p| p == lock));
        assert!(!set.changed(BTreeSet::new));
    }
}
