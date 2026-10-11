//! The oracle over many inputs: the threads, the skip file, the counts
//! and the verdict of a test.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `SILT_ORACLE_FULL=1` | the full sweep: the large step budget, 10,000 generated programs, the `abort` lines run through the `silt` command, the cut inputs held against `cut.txt` |
//! | `SILT_ORACLE_STEPS=<n>` | the step budget of each run |
//! | `SILT_ORACLE_ONLY=<text>` | only the inputs whose name holds the text |
//! | `SILT_ORACLE_WORKERS=<n>` | the number of threads (default: 2) |
//! | `SILT_ORACLE_REPORT=<file>` | append the counts, every finding and the verdict of each input to the file |
//! | `SILT_ORACLE_SEED=<n>` | the seed of the generated programs (default 1) |

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::oracle::{Compared, Cut, Finding, Input, Kind, Source, Steps, Verdict, examine};

/// The threads of a sweep in the suite: the tests of the binary run
/// side by side, and a program with tasks starts a worker for each CPU.
const SUITE_WORKERS: usize = 2;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// The name of the file `path` of the repository: its path from the
/// root, with `/` on every platform.
pub fn name_of(path: &Path) -> String {
    let rel = path.strip_prefix(repo_root()).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

/// Whether `SILT_ORACLE_FULL` asks for the full sweep.
pub fn full() -> bool {
    std::env::var_os("SILT_ORACLE_FULL").is_some_and(|v| v != "0")
}

/// The steps a run may take: in the suite, enough for all but the
/// programs that measure speed; in a full sweep, what such a program
/// takes at slice 1 within the watchdog's time. A program that needs
/// more at both slices is cut short and counted.
const STEPS: u64 = 1_000_000;
const STEPS_FULL: u64 = 20_000_000;

/// The step budgets of each input's runs: `SILT_ORACLE_STEPS`, or the
/// suite's or the full sweep's. A run that is cut short where the other
/// one ended is repeated with the full sweep's budget, or with twice
/// the budget when that is more (a full sweep's own repeat).
pub fn steps() -> Steps {
    let each = match std::env::var("SILT_ORACLE_STEPS") {
        Ok(steps) => steps.parse().expect("SILT_ORACLE_STEPS is a number"),
        Err(_) if full() => STEPS_FULL,
        Err(_) => STEPS,
    };
    Steps {
        each,
        again: STEPS_FULL.max(each.saturating_mul(2)),
    }
}

/// Whether the sweep is the full one with its own budget: the one whose
/// cut inputs `tests/oracle/cut.txt` lists.
fn pinned() -> bool {
    full() && std::env::var_os("SILT_ORACLE_STEPS").is_none()
}

/// One line of the skip file: an input whose finding is known and
/// reported.
pub struct Skip {
    pub input: String,
    /// The finding's kind, as [`crate::oracle::Kind::name`] spells it.
    pub kind: String,
    /// What the finding is and where it was reported.
    pub what: String,
}

/// The lines of the file `name` in `tests/oracle/`, each cut at `|`
/// into `fields` trimmed fields, the last of which says something.
/// Empty lines and lines that start with `#` are no lines.
fn lines_of(name: &str, fields: usize) -> Vec<Vec<String>> {
    let path = repo_root().join("tests/oracle").join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("tests/oracle/{name}: {e}"));
    let mut lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cut: Vec<String> = line
            .splitn(fields, '|')
            .map(|f| f.trim().to_string())
            .collect();
        assert!(
            cut.len() == fields && cut.iter().all(|field| !field.is_empty()),
            "tests/oracle/{name}:{}: not {fields} fields with `|` between them",
            index + 1
        );
        lines.push(cut);
    }
    lines
}

/// The skip file, `tests/oracle/skip.txt`: one line for each input with
/// a known finding, `<input> | <kind> | <what it is>`.
pub fn skips() -> Vec<Skip> {
    let skip = |line: Vec<String>| {
        let [input, kind, what] = <[String; 3]>::try_from(line).expect("three fields");
        Skip { input, kind, what }
    };
    lines_of("skip.txt", 3).into_iter().map(skip).collect()
}

/// `tests/oracle/cut.txt`: the inputs that a full sweep cuts short at
/// both slices, each with why it needs so many steps, `<input> | <why>`.
fn cuts() -> Vec<(String, String)> {
    let cut = |line: Vec<String>| {
        let [input, why] = <[String; 2]>::try_from(line).expect("two fields");
        (input, why)
    };
    lines_of("cut.txt", 2).into_iter().map(cut).collect()
}

/// Fail unless every line of the skip file and of the cut file whose
/// input `belongs` to a class names one of `all`, the inputs of the
/// class: a line that names nothing would never be looked at.
pub fn check_listed(all: &[Input], belongs: impl Fn(&str) -> bool) {
    let skipped = skips().into_iter().map(|skip| ("skip.txt", skip.input));
    let cut = cuts().into_iter().map(|(input, _)| ("cut.txt", input));
    for (file, input) in skipped.chain(cut) {
        assert!(
            !belongs(&input) || all.iter().any(|known| known.name == input),
            "tests/oracle/{file} names {input}, which is no input of the oracle"
        );
    }
}

/// The inputs of `all` a test runs: all of them, or those that
/// `SILT_ORACLE_ONLY` names.
pub fn chosen(all: Vec<Input>) -> Vec<Input> {
    let only = std::env::var("SILT_ORACLE_ONLY").ok();
    all.into_iter()
        .filter(|input| only.as_ref().is_none_or(|only| input.name.contains(only)))
        .collect()
}

/// How long the `silt` command may take with a program that is listed
/// as aborting, before it is taken not to abort any more.
const ABORT_WAIT: Duration = Duration::from_secs(120);

/// The verdict of an input that the skip file lists as ending the
/// process it runs in, which therefore is not run in this one. In a
/// full sweep the `silt` command runs it, and the finding stands while
/// that ends otherwise than with status 0 or 1; in the suite the line is
/// taken at its word.
fn examine_aborting(input: &Input) -> Verdict {
    let listed = |detail: &str| Verdict::Finding(Finding::new(Kind::Abort, detail));
    if !full() {
        return listed("not run here: the skip file says that it ends the process");
    }
    let Source::Memory(files) = &input.source else {
        panic!(
            "{}: only a program of one file can be listed as aborting",
            input.name
        );
    };
    let dir = std::env::temp_dir().join(format!(
        "silt-oracle-{}-{}",
        std::process::id(),
        input.name.replace(['/', '\\', '.'], "_")
    ));
    std::fs::create_dir_all(&dir).expect("a directory for the program");
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("write the program");
    }
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&files[0].0)
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn silt");
    let start = Instant::now();
    let status = loop {
        match child.try_wait().expect("wait for silt") {
            Some(status) => break Some(status),
            None if start.elapsed() > ABORT_WAIT => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let _ = std::fs::remove_dir_all(&dir);
    match status {
        Some(status) if !matches!(status.code(), Some(0 | 1)) => {
            listed(&format!("`silt run` ends with {status}"))
        }
        // The line in the skip file has outlived its finding.
        _ => Verdict::Passed(Compared::Invariants),
    }
}

/// The verdict of each of `inputs`, in their order, and how long it
/// took to reach (which the report file shows, and nothing judges).
/// An input that `skips` lists as aborting is not run in this process
/// ([`examine_aborting`]).
pub fn run(inputs: &[Input], skips: &[Skip]) -> Vec<(Verdict, Duration)> {
    let listed = |input: &Input, kind: Kind| {
        skips
            .iter()
            .any(|skip| skip.input == input.name && skip.kind == kind.name())
    };
    let workers = match std::env::var("SILT_ORACLE_WORKERS") {
        Ok(n) => n.parse().expect("SILT_ORACLE_WORKERS is a number"),
        Err(_) => SUITE_WORKERS,
    };
    let steps = steps();
    let verdicts: Mutex<Vec<Option<(Verdict, Duration)>>> = Mutex::new(vec![None; inputs.len()]);
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers.max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(input) = inputs.get(index) else {
                        break;
                    };
                    // A program that overflows the native stack takes
                    // the process with it: its name is the last one
                    // a thread wrote.
                    eprintln!("oracle: {}", input.name);
                    let start = Instant::now();
                    let verdict = match listed(input, Kind::Abort) {
                        true => examine_aborting(input),
                        false => examine(input, steps),
                    };
                    verdicts.lock().unwrap()[index] = Some((verdict, start.elapsed()));
                }
            });
        }
    });
    let verdicts = verdicts.into_inner().unwrap();
    verdicts
        .into_iter()
        .map(|v| v.expect("a verdict"))
        .collect()
}

/// Count `verdicts`, write the counts, and fail the test unless the
/// findings are exactly those the skip file lists for these inputs:
/// a finding that is not listed is new, and an entry whose input has
/// no such finding any more is to be removed. In the full sweep the
/// inputs that are cut short must be exactly those the cut file lists
/// ([`cuts`]): a program that stops ending is cut short too.
pub fn conclude(what: &str, inputs: &[Input], verdicts: &[(Verdict, Duration)], skips: &[Skip]) {
    let mut not_run: BTreeMap<String, usize> = BTreeMap::new();
    let mut cut: BTreeMap<Cut, usize> = BTreeMap::new();
    let mut passed: BTreeMap<Compared, usize> = BTreeMap::new();
    let mut listed = Vec::new();
    let mut new = Vec::new();
    let mut stale = Vec::new();
    let mut unpinned = Vec::new();
    let cut_file = cuts();
    for (input, (verdict, _)) in inputs.iter().zip(verdicts) {
        if pinned() {
            let listed = cut_file.iter().find(|(name, _)| *name == input.name);
            match (verdict, listed) {
                (Verdict::Cut(why), None) => unpinned.push(format!(
                    "{}: cut short ({why}) and not in tests/oracle/cut.txt",
                    input.name
                )),
                (Verdict::Cut(_), Some(_)) | (_, None) => {}
                (_, Some((_, why))) => unpinned.push(format!(
                    "{}: in tests/oracle/cut.txt ({why}) and not cut short",
                    input.name
                )),
            }
        }
        let entry = skips.iter().find(|skip| skip.input == input.name);
        let finding = match verdict {
            Verdict::NotRun(why) => {
                *not_run.entry(why.to_string()).or_default() += 1;
                None
            }
            Verdict::Cut(why) => {
                *cut.entry(*why).or_default() += 1;
                None
            }
            Verdict::Passed(compared) => {
                *passed.entry(*compared).or_default() += 1;
                None
            }
            Verdict::Finding(finding) => Some(finding),
        };
        match (finding, entry) {
            (Some(finding), Some(skip)) if skip.kind == finding.kind.name() => {
                listed.push(format!("{} [{}] {}", input.name, skip.kind, skip.what));
            }
            (Some(finding), _) => new.push(format!(
                "{} [{}]\n  {}",
                input.name,
                finding.kind.name(),
                finding.detail.replace('\n', "\n  ")
            )),
            (None, Some(skip)) => stale.push(format!(
                "{} [{}] {}: the oracle finds nothing of the kind",
                input.name, skip.kind, skip.what
            )),
            (None, None) => {}
        }
    }

    let mut report = format!(
        "oracle, {what}: {} inputs, {} steps a run\n",
        inputs.len(),
        steps().each
    );
    let count = |compared| passed.get(&compared).copied().unwrap_or(0);
    report.push_str(&format!(
        "  passed, everything compared: {}\n  passed, invariants only: {}\n",
        count(Compared::Everything),
        count(Compared::Invariants),
    ));
    for (why, count) in &cut {
        report.push_str(&format!("  cut short, {why}: {count}\n"));
    }
    for (why, count) in &not_run {
        report.push_str(&format!("  not run, {why}: {count}\n"));
    }
    for (title, lines) in [
        ("findings the skip file lists", &listed),
        ("NEW findings", &new),
        ("skip entries to remove", &stale),
        ("cut short otherwise than the cut file says", &unpinned),
    ] {
        report.push_str(&format!("  {title}: {}\n", lines.len()));
        for line in lines {
            report.push_str(&format!("    {}\n", line.replace('\n', "\n    ")));
        }
    }
    eprint!("{report}");
    if let Ok(path) = std::env::var("SILT_ORACLE_REPORT") {
        use std::io::Write;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path);
        let mut file = file.unwrap_or_else(|e| panic!("cannot open {path}: {e}"));
        // The file has the verdict of each input too.
        let mut lines = report.clone();
        for (input, (verdict, took)) in inputs.iter().zip(verdicts) {
            let verdict = match verdict {
                Verdict::NotRun(why) => format!("not run, {why}"),
                Verdict::Cut(why) => format!("cut short, {why}"),
                Verdict::Passed(compared) => format!("passed, {compared:?}"),
                Verdict::Finding(finding) => format!("FINDING, {}", finding.kind.name()),
            };
            let ms = took.as_millis();
            lines.push_str(&format!("    {}: {verdict} ({ms} ms)\n", input.name));
        }
        file.write_all(lines.as_bytes()).expect("write the report");
    }
    assert!(
        new.is_empty() && stale.is_empty() && unpinned.is_empty(),
        "the oracle's findings are not those of tests/oracle/skip.txt, or its cut inputs \
         not those of tests/oracle/cut.txt ({} new, {} entries to remove, {} cut otherwise):\n\
         {report}",
        new.len(),
        stale.len(),
        unpinned.len()
    );
}
