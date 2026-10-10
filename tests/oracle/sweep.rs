//! The oracle over many inputs: the threads, the skip file, the counts
//! and the verdict of a test.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `SILT_ORACLE_FULL=1` | every input of a class instead of its sample |
//! | `SILT_ORACLE_ONLY=<text>` | only the inputs whose name holds the text |
//! | `SILT_ORACLE_WORKERS=<n>` | the number of threads (default: 2) |
//! | `SILT_ORACLE_REPORT=<file>` | append the counts, every finding and the verdict of each input to the file |
//! | `SILT_ORACLE_SEED=<n>` | the seed of the generated programs (default 1) |

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::oracle::{Compared, Input, Verdict, examine};

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

/// Whether `SILT_ORACLE_FULL` asks for every input.
pub fn full() -> bool {
    std::env::var_os("SILT_ORACLE_FULL").is_some_and(|v| v != "0")
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

/// The skip file, `tests/oracle/skip.txt`: one line for each input with
/// a known finding, `<input> | <kind> | <what it is>`. Empty lines and
/// lines that start with `#` say nothing.
pub fn skips() -> Vec<Skip> {
    let path = repo_root().join("tests/oracle/skip.txt");
    let text = std::fs::read_to_string(&path).expect("tests/oracle/skip.txt");
    let mut skips = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.splitn(3, '|').map(str::trim).collect();
        let [input, kind, what] = fields[..] else {
            panic!(
                "tests/oracle/skip.txt:{}: not `<input> | <kind> | <what>`",
                index + 1
            );
        };
        assert!(
            !what.is_empty(),
            "tests/oracle/skip.txt:{}: the entry does not say what the finding is",
            index + 1
        );
        skips.push(Skip {
            input: input.to_string(),
            kind: kind.to_string(),
            what: what.to_string(),
        });
    }
    skips
}

/// The inputs of `all` a test runs: all of them when the sweep is full,
/// else every `step`-th and each one the skip file names (a listed
/// finding is looked at in every run, so its entry cannot outlive it).
/// `SILT_ORACLE_ONLY` narrows either.
pub fn sample(all: Vec<Input>, step: usize, skips: &[Skip]) -> Vec<Input> {
    let only = std::env::var("SILT_ORACLE_ONLY").ok();
    let full = full();
    all.into_iter()
        .enumerate()
        .filter(|(index, input)| {
            full || index % step == 0 || skips.iter().any(|skip| skip.input == input.name)
        })
        .map(|(_, input)| input)
        .filter(|input| only.as_ref().is_none_or(|only| input.name.contains(only)))
        .collect()
}

/// The verdict of each of `inputs`, in their order, and how long it
/// took to reach (which the report file shows, and nothing judges).
pub fn run(inputs: &[Input]) -> Vec<(Verdict, Duration)> {
    let workers = match std::env::var("SILT_ORACLE_WORKERS") {
        Ok(n) => n.parse().expect("SILT_ORACLE_WORKERS is a number"),
        Err(_) => SUITE_WORKERS,
    };
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
                    let start = Instant::now();
                    let verdict = examine(input);
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
/// no such finding any more is to be removed.
pub fn conclude(what: &str, inputs: &[Input], verdicts: &[(Verdict, Duration)], skips: &[Skip]) {
    let mut not_run: BTreeMap<String, usize> = BTreeMap::new();
    let mut passed: BTreeMap<Compared, usize> = BTreeMap::new();
    let mut listed = Vec::new();
    let mut new = Vec::new();
    let mut stale = Vec::new();
    for (input, (verdict, _)) in inputs.iter().zip(verdicts) {
        let entry = skips.iter().find(|skip| skip.input == input.name);
        let finding = match verdict {
            Verdict::NotRun(why) => {
                *not_run.entry(why.to_string()).or_default() += 1;
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

    let mut report = format!("oracle, {what}: {} inputs\n", inputs.len());
    let count = |compared| passed.get(&compared).copied().unwrap_or(0);
    report.push_str(&format!(
        "  passed, everything compared: {}\n  passed, invariants only: {}\n",
        count(Compared::Everything),
        count(Compared::Invariants),
    ));
    for (why, count) in &not_run {
        report.push_str(&format!("  not run, {why}: {count}\n"));
    }
    for (title, lines) in [
        ("findings the skip file lists", &listed),
        ("NEW findings", &new),
        ("skip entries to remove", &stale),
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
                Verdict::Passed(compared) => format!("passed, {compared:?}"),
                Verdict::Finding(finding) => format!("FINDING, {}", finding.kind.name()),
            };
            let ms = took.as_millis();
            lines.push_str(&format!("    {}: {verdict} ({ms} ms)\n", input.name));
        }
        file.write_all(lines.as_bytes()).expect("write the report");
    }
    assert!(
        new.is_empty() && stale.is_empty(),
        "the oracle's findings are not those of tests/oracle/skip.txt \
         ({} new, {} entries to remove):\n{report}",
        new.len(),
        stale.len()
    );
}
