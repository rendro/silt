//! Golden tests: silt programs under `tests/golden/<area>/` with the
//! output they must produce, run through the built `silt` binary as a user
//! would run them. The format is described in `tests/golden/README.md`.
//!
//! The `golden_shard_*` tests walk every case, run them in parallel and
//! report every failure at once. `SILT_GOLDEN_FILTER=<text>` runs only the
//! cases whose path contains the text; `SILT_BLESS=1` rewrites the
//! existing `.stdout` and `.stderr` files and `-- verdict:` marks from the
//! current binary.
//!
//! The `verdict_shard_*` tests run the cases that carry a `-- verdict:`
//! mark through `check`, `run`, `test` and the LSP and compare the static
//! diagnostics of the four (see `verdict.rs`). The imported repro corpus
//! under `repros/` is verdict-only; a fixed sample of it runs by default,
//! all of it with `SILT_GOLDEN_FULL_CORPUS=1`.

mod lsp;
mod soundness;
mod verdict;

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
    /// The file the directives are read from.
    source_path: PathBuf,
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
    verdict: Option<verdict::Mark>,
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
            "verdict" => d.verdict = Some(verdict::Mark::parse(&value)?),
            // An ordinary comment that happens to contain a colon.
            _ => {}
        }
    }
    // The LSP is a cargo feature; a case that talks to it needs it.
    if d.cmd.first().map(String::as_str) == Some("lsp")
        && !d.requires_features.iter().any(|f| f == "lsp")
    {
        d.requires_features.push("lsp".to_string());
    }
    Ok(d)
}

/// The directives of a verdict-only case (one under `repros/`): only
/// `-- verdict:` is read, since the rest of the leading comment block is
/// the imported program's own comments.
fn parse_verdict_only(source: &str) -> Result<Directives, String> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut d = Directives {
        timeout: CASE_TIMEOUT,
        ..Directives::default()
    };
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("--") else {
            break;
        };
        if let Some((key, value)) = rest.trim().split_once(':')
            && key.trim() == "verdict"
        {
            d.verdict = Some(verdict::Mark::parse(value.trim())?);
        }
    }
    if d.verdict.is_none() {
        return Err("a case under repros/ needs a `-- verdict:` mark".to_string());
    }
    Ok(d)
}

/// Rewrite the `-- verdict:` line of the case whose directives are read
/// from `source_path` to say `mark`.
fn bless_verdict(source_path: &Path, mark: &verdict::Mark) {
    let source = std::fs::read(source_path).expect("read case for bless");
    let text = String::from_utf8_lossy(&source);
    let mut out = String::with_capacity(text.len());
    let mut done = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start_matches('\u{feff}').trim();
        if !done
            && let Some(rest) = trimmed.strip_prefix("--")
            && rest.trim().starts_with("verdict:")
        {
            let eol = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
            let bom = if line.starts_with('\u{feff}') {
                "\u{feff}"
            } else {
                ""
            };
            out.push_str(&format!("{bom}-- verdict: {mark}{eol}"));
            done = true;
        } else {
            out.push_str(line);
        }
    }
    std::fs::write(source_path, out).expect("write blessed verdict");
}

impl Case {
    /// The entry file, relative to `dir`: what the LSP opens and the
    /// verdict's doors are given.
    fn entry(&self) -> String {
        if self.file.is_empty() {
            "src/main.silt".to_string()
        } else {
            self.file.clone()
        }
    }
}

/// Whether `path` is a verdict-only case: one under `repros/`.
fn is_verdict_only(path: &Path) -> bool {
    path.strip_prefix(golden_root())
        .is_ok_and(|rel| rel.starts_with(REPROS))
}

/// The directory, under the golden root, of the imported repro corpus.
const REPROS: &str = "repros";

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
    let bytes = std::fs::read(&source_path)
        .map_err(|e| format!("cannot read {}: {e}", source_path.display()))?;
    // A verdict-only case may be any text the lexer is to reject; its
    // directives are plain ASCII all the same.
    let source = String::from_utf8_lossy(&bytes);
    let directives = if is_verdict_only(path) {
        parse_verdict_only(&source)?
    } else {
        parse_directives(&source)?
    };
    Ok(Case {
        is_dir: path.is_dir(),
        source_path,
        dir,
        file,
        expected_base,
        directives,
    })
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
    /// A failure of the harness's exchange with the binary (an LSP
    /// session that published nothing for the file).
    harness_error: Option<String>,
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
        // A lockfile in a case directory is left over from running the
        // case by hand; it pins dependencies to absolute paths in the
        // tree, so it is never copied.
        if entry.file_name() == "silt.lock" {
            continue;
        }
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
    if case.directives.cmd.first().map(String::as_str) == Some("lsp") {
        let session = lsp::session(dir, &case.entry(), case.directives.timeout);
        return Output {
            code: session.code,
            stdout: session.render(&case.entry()),
            stderr: session.stderr,
            timed_out: false,
            harness_error: session.error,
        };
    }
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
        harness_error: None,
    }
}

/// On Windows, the backslashes inside paths to `.silt` files and to
/// package files (`silt.toml`, `silt.lock`) become `/`, so one expected
/// file serves every platform. Elsewhere the text is unchanged.
fn portable_paths(text: &str) -> String {
    if !cfg!(windows) {
        return text.to_string();
    }
    text.split_inclusive(|c: char| c.is_whitespace() || c == '`' || c == '\'')
        .map(|token| {
            match [".silt", "silt.toml", "silt.lock"]
                .iter()
                .filter_map(|file| token.rfind(file))
                .max()
            {
                // Only the path part, before the file name's end: a JSON
                // string in the same token may hold escapes such as `\\n`.
                // A path inside JSON has its separators escaped (`\\\\`).
                Some(end) => format!(
                    "{}{}",
                    token[..end].replace("\\\\", "/").replace('\\', "/"),
                    &token[end..]
                ),
                None => token.to_string(),
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
        harness_error: out.harness_error.clone(),
    };
    let d = &case.directives;
    let mut problems = Vec::new();
    if out.timed_out {
        problems.push(format!("did not exit within {:?}", d.timeout));
        return problems;
    }
    if let Some(e) = &out.harness_error {
        problems.push(e.clone());
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
    problems.extend(unlocated_errors(&out.stderr));
    if !problems.is_empty() {
        problems.push(format!(
            "stdout was:\n{}\nstderr was:\n{}",
            out.stdout, out.stderr
        ));
    }
    problems
}

/// The error diagnostics in `stderr` that render without a ` --> ` line:
/// every diagnostic has a span, so every one shows where it is. Headers
/// indented under a test result line count too. An `error[fmt]` refusal
/// is not a diagnostic about the program and is left out. The verdict
/// mode applies this to `check`'s stderr of every verdict case, the
/// repro corpus included.
fn unlocated_errors(stderr: &str) -> Vec<String> {
    let lines: Vec<&str> = stderr.lines().collect();
    let mut problems = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some((true, kind, _)) = verdict::header(line.trim_start()) else {
            continue;
        };
        if kind == "fmt" {
            continue;
        }
        let located = lines
            .get(i + 1)
            .is_some_and(|next| next.trim_start().starts_with("--> "));
        if !located {
            problems.push(format!("error diagnostic without a location: {line}"));
        }
    }
    problems
}

/// The corpus is split into this many tests, each running every
/// `SHARDS`-th case of the sorted list, so a test runner can spread the
/// cases over its workers and CI partitions stay balanced.
const SHARDS: usize = 8;

/// How many cases of the repro corpus the verdict shards run by default.
const REPRO_SAMPLE: usize = 200;

macro_rules! shards {
    ($run:ident: $($name:ident = $k:expr),* $(,)?) => {
        $(#[test] fn $name() { $run($k); })*
    };
}

shards!(
    run_shard:
    golden_shard_0 = 0,
    golden_shard_1 = 1,
    golden_shard_2 = 2,
    golden_shard_3 = 3,
    golden_shard_4 = 4,
    golden_shard_5 = 5,
    golden_shard_6 = 6,
    golden_shard_7 = 7,
);

shards!(
    run_verdict_shard:
    verdict_shard_0 = 0,
    verdict_shard_1 = 1,
    verdict_shard_2 = 2,
    verdict_shard_3 = 3,
    verdict_shard_4 = 4,
    verdict_shard_5 = 5,
    verdict_shard_6 = 6,
    verdict_shard_7 = 7,
);

/// Every case, sorted, narrowed by `SILT_GOLDEN_FILTER`.
fn all_cases() -> Vec<PathBuf> {
    let mut all = Vec::new();
    collect_cases(&golden_root(), &mut all);
    if let Ok(filter) = std::env::var("SILT_GOLDEN_FILTER") {
        all.retain(|p| p.to_string_lossy().contains(&filter));
    }
    all
}

/// The `shard`-th of `SHARDS` slices of `all`.
fn shard_of(all: Vec<PathBuf>, shard: usize) -> Vec<PathBuf> {
    all.into_iter()
        .enumerate()
        .filter(|(i, _)| i % SHARDS == shard)
        .map(|(_, p)| p)
        .collect()
}

fn run_shard(shard: usize) {
    let all = all_cases()
        .into_iter()
        .filter(|p| !is_verdict_only(p))
        .collect();
    let bless = std::env::var_os("SILT_BLESS").is_some();
    run_cases(&shard_of(all, shard), "golden", |case| {
        for run in 1..=case.directives.repeat {
            let out = run_case(case);
            let problems = judge(case, &out, bless);
            if !problems.is_empty() {
                return vec![format!(
                    "(run {run} of {}):\n  {}",
                    case.directives.repeat,
                    problems.join("\n  ")
                )];
            }
        }
        Vec::new()
    });
}

/// The verdict cases: every case outside `repros/` that carries a
/// `-- verdict:` mark, and the repro corpus (all of it with
/// `SILT_GOLDEN_FULL_CORPUS=1`, else every n-th case for a sample of
/// `REPRO_SAMPLE`).
fn verdict_cases() -> Vec<PathBuf> {
    let (repros, others): (Vec<PathBuf>, Vec<PathBuf>) =
        all_cases().into_iter().partition(|p| is_verdict_only(p));
    let full = std::env::var_os("SILT_GOLDEN_FULL_CORPUS").is_some_and(|v| v != "0");
    let repros: Vec<PathBuf> = if full || repros.len() <= REPRO_SAMPLE {
        repros
    } else {
        (0..REPRO_SAMPLE)
            .map(|i| repros[i * repros.len() / REPRO_SAMPLE].clone())
            .collect()
    };
    let mut cases: Vec<PathBuf> = others
        .into_iter()
        .filter(|p| load_case(p).is_ok_and(|c| c.directives.verdict.is_some()))
        .chain(repros)
        .collect();
    cases.sort();
    cases
}

/// Whether this build has every cargo feature. The verdict marks are
/// recorded against an `--all-features` build: without a feature, a
/// program that uses it gets other diagnostics.
fn all_features() -> bool {
    [
        "repl",
        "lsp",
        "watch",
        "local-clock",
        "http",
        "tcp",
        "tcp-tls",
        "postgres",
        "postgres-tls",
    ]
    .iter()
    .all(|f| feature_enabled(f).unwrap_or(false))
}

fn run_verdict_shard(shard: usize) {
    if !all_features() {
        eprintln!("verdict cases skipped: they need a build with --all-features");
        return;
    }
    if std::env::var_os("SILT_GOLDEN_SKIP_VERDICT").is_some_and(|v| v != "0") {
        eprintln!("verdict cases skipped: SILT_GOLDEN_SKIP_VERDICT is set");
        return;
    }
    let bless = std::env::var_os("SILT_BLESS").is_some();
    run_cases(&shard_of(verdict_cases(), shard), "verdict", |case| {
        let Some(mark) = &case.directives.verdict else {
            return Vec::new();
        };
        let (verdicts, unlocated) = verdict::verdicts(
            &|| scratch_copy(case),
            &case.entry(),
            case.directives.timeout,
        );
        if bless {
            let actual = verdict::mark_for(&verdicts);
            if &actual != mark {
                bless_verdict(&case.source_path, &actual);
            }
            return unlocated;
        }
        let mut problems = verdict::judge(mark, &verdicts);
        problems.extend(unlocated);
        problems
    });
}

/// Run `check` on every case of `paths` in parallel and fail with every
/// problem it reports. `what` names the cases in messages.
fn run_cases(paths: &[PathBuf], what: &str, check: impl Fn(&Case) -> Vec<String> + Sync) {
    let root = golden_root();
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
                    let problems = check(&case);
                    if !problems.is_empty() {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("{rel} {}", problems.join("\n  ")));
                    }
                }
            });
        }
    });

    let skipped = skipped.into_inner().unwrap();
    if !skipped.is_empty() {
        eprintln!(
            "{} {what} cases skipped for features this build lacks:\n  {}",
            skipped.len(),
            skipped.join("\n  ")
        );
    }
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} {what} cases failed:\n\n{}",
        failures.len(),
        paths.len(),
        failures.join("\n\n")
    );
}
