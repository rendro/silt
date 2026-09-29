//! Golden tests: silt programs under `tests/golden/<area>/` with the
//! output they must produce, run through the built `silt` binary as a user
//! would run them. The format is described in `tests/golden/README.md`.
//!
//! One test walks every case, runs them in parallel and reports every
//! failure at once. `SILT_GOLDEN_FILTER=<text>` runs only the cases whose
//! path contains the text; `SILT_BLESS=1` rewrites the existing `.stdout`
//! and `.stderr` files from the current binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A case that has not exited after this long has failed.
const CASE_TIMEOUT: Duration = Duration::from_secs(20);

/// One golden case: the program to run and what it must produce.
struct Case {
    /// Whether the case is a directory (a multi-file case).
    is_dir: bool,
    /// The directory the binary runs in.
    dir: PathBuf,
    /// The file name passed to the binary, relative to `dir`.
    file: String,
    /// Where the expected output files live, without extension.
    expected_base: PathBuf,
    directives: Directives,
}

#[derive(Default)]
struct Directives {
    cmd: Vec<String>,
    exit: i32,
    stdout_contains: Vec<String>,
    stderr_contains: Vec<String>,
    stderr_not_contains: Vec<String>,
    stdin: String,
    repeat: usize,
    timeout: Duration,
    requires_features: Vec<String>,
}

/// Whether the cargo feature `name` is enabled. The golden test binary
/// is built with the same features as the `silt` binary it runs.
fn feature_enabled(name: &str) -> Result<bool, String> {
    Ok(match name {
        "repl" => cfg!(feature = "repl"),
        "lsp" => cfg!(feature = "lsp"),
        "watch" => cfg!(feature = "watch"),
        "local-clock" => cfg!(feature = "local-clock"),
        "http" => cfg!(feature = "http"),
        "tcp" => cfg!(feature = "tcp"),
        "tcp-tls" => cfg!(feature = "tcp-tls"),
        "postgres" => cfg!(feature = "postgres"),
        "postgres-tls" => cfg!(feature = "postgres-tls"),
        other => {
            return Err(format!(
                "unknown feature {other:?} in `-- requires-feature:`"
            ));
        }
    })
}

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Every case under `root`: each `.silt` file outside a case directory,
/// and each directory that holds a `main.silt`.
fn collect_cases(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.join("main.silt").is_file() || is_package_case(&path) {
                out.push(path);
            } else {
                collect_cases(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "silt") {
            out.push(path);
        }
    }
}

/// A package case: a directory with a `silt.toml` and `src/main.silt`
/// and no `main.silt` of its own. It runs as `silt <cmd>` with no file,
/// the way a user runs a package, and its directives are read from
/// `src/main.silt`.
fn is_package_case(dir: &Path) -> bool {
    !dir.join("main.silt").is_file()
        && dir.join("silt.toml").is_file()
        && dir.join("src/main.silt").is_file()
}

fn parse_directives(source: &str) -> Result<Directives, String> {
    // A byte-order mark at the start of the file is not part of the first
    // directive.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut d = Directives {
        cmd: vec!["run".to_string()],
        repeat: 1,
        timeout: CASE_TIMEOUT,
        ..Directives::default()
    };
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("--") else {
            if line.is_empty() {
                continue;
            }
            break;
        };
        let Some((key, value)) = rest.trim().split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "cmd" => d.cmd = value.split_whitespace().map(str::to_string).collect(),
            "exit" => {
                d.exit = value
                    .parse()
                    .map_err(|_| format!("bad `-- exit:` value {value:?}"))?
            }
            "stdout-contains" => d.stdout_contains.push(value),
            "stderr-contains" => d.stderr_contains.push(value),
            "stderr-not-contains" => d.stderr_not_contains.push(value),
            "stdin" => d.stdin = value.replace("\\n", "\n"),
            "requires-feature" => d.requires_features.push(value),
            "timeout" => {
                let secs: u64 = value
                    .parse()
                    .map_err(|_| format!("bad `-- timeout:` value {value:?}"))?;
                d.timeout = Duration::from_secs(secs);
            }
            "repeat" => {
                d.repeat = value
                    .parse()
                    .map_err(|_| format!("bad `-- repeat:` value {value:?}"))?
            }
            // An ordinary comment that happens to contain a colon.
            _ => {}
        }
    }
    Ok(d)
}

fn load_case(path: &Path) -> Result<Case, String> {
    let (dir, file, source_path, expected_base) = if path.is_dir() && is_package_case(path) {
        (
            path.to_path_buf(),
            String::new(),
            path.join("src/main.silt"),
            path.join("case"),
        )
    } else if path.is_dir() {
        (
            path.to_path_buf(),
            "main.silt".to_string(),
            path.join("main.silt"),
            path.join("case"),
        )
    } else {
        let dir = path
            .parent()
            .expect("a case file has a parent")
            .to_path_buf();
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        (dir, file, path.to_path_buf(), path.with_extension(""))
    };
    let source = std::fs::read_to_string(&source_path)
        .map_err(|e| format!("cannot read {}: {e}", source_path.display()))?;
    Ok(Case {
        is_dir: path.is_dir(),
        dir,
        file,
        expected_base,
        directives: parse_directives(&source)?,
    })
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

/// A fresh directory holding a copy of the case: the whole directory of a
/// multi-file case, or the one file of a single-file case. The case runs
/// there, so nothing it or `silt` writes (a lockfile, an output file)
/// lands in the source tree.
fn scratch_copy(case: &Case) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt-golden-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    if case.is_dir {
        copy_dir(&case.dir, &dir);
    } else {
        std::fs::copy(case.dir.join(&case.file), dir.join(&case.file)).expect("copy case file");
    }
    dir
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).expect("read case dir").flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            std::fs::create_dir_all(&dst).expect("create dir");
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).expect("copy file");
        }
    }
}

fn run_case(case: &Case) -> Output {
    let scratch = scratch_copy(case);
    let out = run_in(case, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    out
}

fn run_in(case: &Case, dir: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_silt"));
    command.args(&case.directives.cmd);
    // A REPL session reads its input from stdin, not from the file; the
    // file holds only the directives and the session's description.
    if case.directives.cmd.first().map(String::as_str) != Some("repl") && !case.file.is_empty() {
        command.arg(&case.file);
    }
    command
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn silt");
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin");
        let _ = stdin.write_all(case.directives.stdin.as_bytes());
    }
    // Read the pipes on their own threads so a large output cannot block
    // the child while this thread waits for it.
    let mut out_pipe = child.stdout.take().expect("stdout");
    let mut err_pipe = child.stderr.take().expect("stderr");
    let out_reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = std::io::Read::read_to_end(&mut out_pipe, &mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = std::io::Read::read_to_end(&mut err_pipe, &mut s);
        s
    });
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if started.elapsed() >= case.directives.timeout => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().expect("wait after kill");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    Output {
        code: status.code(),
        stdout: String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).into_owned(),
        timed_out,
    }
}

/// On Windows, the backslashes inside paths to `.silt` files become `/`,
/// so one expected file serves every platform. Elsewhere the text is
/// unchanged.
fn portable_paths(text: &str) -> String {
    if !cfg!(windows) {
        return text.to_string();
    }
    text.split_inclusive(|c: char| c.is_whitespace() || c == '`' || c == '\'')
        .map(|token| {
            if token.contains(".silt") {
                token.replace('\\', "/")
            } else {
                token.to_string()
            }
        })
        .collect()
}

/// The problems with `out` for `case`, empty when it passes.
fn judge(case: &Case, out: &Output, bless: bool) -> Vec<String> {
    let out = &Output {
        code: out.code,
        stdout: portable_paths(&out.stdout),
        stderr: portable_paths(&out.stderr),
        timed_out: out.timed_out,
    };
    let d = &case.directives;
    let mut problems = Vec::new();
    if out.timed_out {
        problems.push(format!("did not exit within {:?}", d.timeout));
        return problems;
    }
    if out.code != Some(d.exit) {
        problems.push(format!("exit status {:?}, expected {}", out.code, d.exit));
    }
    for (ext, actual) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        let expected_path = case.expected_base.with_extension(ext);
        if !expected_path.is_file() {
            continue;
        }
        if bless {
            std::fs::write(&expected_path, actual).expect("write blessed output");
            continue;
        }
        let expected = std::fs::read_to_string(&expected_path).unwrap_or_default();
        if &expected != actual {
            problems.push(format!(
                "{ext} differs from {}:\n--- expected\n{expected}--- actual\n{actual}---",
                expected_path.display()
            ));
        }
    }
    for needle in &d.stdout_contains {
        if !out.stdout.contains(needle.as_str()) {
            problems.push(format!("stdout does not contain {needle:?}"));
        }
    }
    for needle in &d.stderr_contains {
        if !out.stderr.contains(needle.as_str()) {
            problems.push(format!("stderr does not contain {needle:?}"));
        }
    }
    for needle in &d.stderr_not_contains {
        if out.stderr.contains(needle.as_str()) {
            problems.push(format!("stderr contains {needle:?}"));
        }
    }
    if !problems.is_empty() {
        problems.push(format!(
            "stdout was:\n{}\nstderr was:\n{}",
            out.stdout, out.stderr
        ));
    }
    problems
}

/// The corpus is split into this many tests, each running every
/// `SHARDS`-th case of the sorted list, so a test runner can spread the
/// cases over its workers and CI partitions stay balanced.
const SHARDS: usize = 8;

macro_rules! shards {
    ($($name:ident = $k:expr),* $(,)?) => {
        $(#[test] fn $name() { run_shard($k); })*
    };
}

shards!(
    golden_shard_0 = 0,
    golden_shard_1 = 1,
    golden_shard_2 = 2,
    golden_shard_3 = 3,
    golden_shard_4 = 4,
    golden_shard_5 = 5,
    golden_shard_6 = 6,
    golden_shard_7 = 7,
);

fn run_shard(shard: usize) {
    let root = golden_root();
    let mut all = Vec::new();
    collect_cases(&root, &mut all);
    if let Ok(filter) = std::env::var("SILT_GOLDEN_FILTER") {
        all.retain(|p| p.to_string_lossy().contains(&filter));
    }
    let paths: Vec<PathBuf> = all
        .into_iter()
        .enumerate()
        .filter(|(i, _)| i % SHARDS == shard)
        .map(|(_, p)| p)
        .collect();
    let bless = std::env::var_os("SILT_BLESS").is_some();

    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let skipped: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    // The shards may run side by side (threads under `cargo test`,
    // processes under nextest), so each takes a share of the CPUs.
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .div_ceil(4)
        .max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some(path) = paths.get(i) else { break };
                    let rel = path
                        .strip_prefix(&root)
                        .unwrap_or(path)
                        .display()
                        .to_string();
                    let case = match load_case(path) {
                        Ok(case) => case,
                        Err(e) => {
                            failures.lock().unwrap().push(format!("{rel}: {e}"));
                            continue;
                        }
                    };
                    let mut missing = Vec::new();
                    for feature in &case.directives.requires_features {
                        match feature_enabled(feature) {
                            Ok(true) => {}
                            Ok(false) => missing.push(feature.clone()),
                            Err(e) => {
                                failures.lock().unwrap().push(format!("{rel}: {e}"));
                                missing.push(feature.clone());
                            }
                        }
                    }
                    if !missing.is_empty() {
                        skipped
                            .lock()
                            .unwrap()
                            .push(format!("{rel} (needs {})", missing.join(", ")));
                        continue;
                    }
                    for run in 1..=case.directives.repeat {
                        let out = run_case(&case);
                        let problems = judge(&case, &out, bless);
                        if !problems.is_empty() {
                            failures.lock().unwrap().push(format!(
                                "{rel} (run {run} of {}):\n  {}",
                                case.directives.repeat,
                                problems.join("\n  ")
                            ));
                            break;
                        }
                    }
                }
            });
        }
    });

    let skipped = skipped.into_inner().unwrap();
    if !skipped.is_empty() {
        eprintln!(
            "{} golden cases skipped for features this build lacks:\n  {}",
            skipped.len(),
            skipped.join("\n  ")
        );
    }
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} golden cases failed:\n\n{}",
        failures.len(),
        paths.len(),
        failures.join("\n\n")
    );
}
