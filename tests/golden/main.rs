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
            if path.join("main.silt").is_file() {
                out.push(path);
            } else {
                collect_cases(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "silt") {
            out.push(path);
        }
    }
}

fn parse_directives(source: &str) -> Result<Directives, String> {
    let mut d = Directives {
        cmd: vec!["run".to_string()],
        repeat: 1,
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
    let (dir, file, source_path, expected_base) = if path.is_dir() {
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

fn run_case(case: &Case) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_silt"));
    command
        .args(&case.directives.cmd)
        .arg(&case.file)
        .current_dir(&case.dir)
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
            None if started.elapsed() >= CASE_TIMEOUT => {
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

/// The problems with `out` for `case`, empty when it passes.
fn judge(case: &Case, out: &Output, bless: bool) -> Vec<String> {
    let d = &case.directives;
    let mut problems = Vec::new();
    if out.timed_out {
        problems.push(format!("did not exit within {CASE_TIMEOUT:?}"));
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

#[test]
fn golden_cases() {
    let root = golden_root();
    let mut paths = Vec::new();
    collect_cases(&root, &mut paths);
    if let Ok(filter) = std::env::var("SILT_GOLDEN_FILTER") {
        paths.retain(|p| p.to_string_lossy().contains(&filter));
    }
    let bless = std::env::var_os("SILT_BLESS").is_some();

    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
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

    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} golden cases failed:\n\n{}",
        failures.len(),
        paths.len(),
        failures.join("\n\n")
    );
}
