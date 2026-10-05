//! The stage 6 exit manifest, `repros/SOUNDNESS.tsv`: what `silt check`
//! must say about every program of the two soundness repro directories
//! and of `lang/soundness/` when stage 6 is done. The `soundness_manifest_*`
//! tests check each row against `check --format json`. A row marked
//! `pending:<step>` does not hold yet: the test passes while it fails and
//! fails once it holds, so the step that fixes it removes the mark. The
//! format is described in `tests/golden/README.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use silt::diagnostic::{Code, Phase};

use super::{Case, Directives, Output};

/// The manifest, under the golden root.
const MANIFEST: &str = "repros/SOUNDNESS.tsv";

/// The directories whose every case has a row.
const DIRS: [&str; 3] = [
    "repros/type_soundness",
    "repros/typechecker_arch",
    "lang/soundness",
];

/// What a row says of its program.
enum Expect {
    /// `check` reports an error with this code.
    Reject(Code),
    /// `check` reports no error.
    Accept,
    /// `check` reports no error, and `run` prints the case's `.stdout`.
    Output,
    /// The program says nothing of the checker any more (a removed
    /// feature, or syntax the parser refuses). Not run.
    Obsolete,
    /// What the program should do is not settled. Not run.
    Undetermined,
}

struct Row {
    /// The line of the manifest, for messages.
    line: usize,
    /// The stage 6 step expected to make the row hold, for a row that
    /// does not hold yet.
    pending: Option<String>,
    expect: Expect,
}

/// The rows by case path (relative to the golden root, `/`-separated),
/// and the problems of the manifest itself.
fn parse_manifest(text: &str) -> (BTreeMap<String, Row>, Vec<String>) {
    let mut rows = BTreeMap::new();
    let mut problems = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let [case, state, expect, _note] = fields[..] else {
            problems.push(format!(
                "line {line_no}: expected 4 tab-separated fields (case, state, expect, note), found {}",
                fields.len()
            ));
            continue;
        };
        let expect = match expect.split_whitespace().collect::<Vec<_>>()[..] {
            ["reject", id] => match Code::ALL.iter().find(|c| c.id() == id) {
                Some(code) => Expect::Reject(*code),
                None => {
                    problems.push(format!("line {line_no}: no diagnostic code {id:?}"));
                    continue;
                }
            },
            ["accept"] => Expect::Accept,
            ["output"] => Expect::Output,
            ["obsolete"] => Expect::Obsolete,
            ["undetermined"] => Expect::Undetermined,
            _ => {
                problems.push(format!(
                    "line {line_no}: expect is {expect:?}, not `reject <code>`, `accept`, `output`, `obsolete` or `undetermined`"
                ));
                continue;
            }
        };
        let checked = !matches!(expect, Expect::Obsolete | Expect::Undetermined);
        let pending = match (state, state.strip_prefix("pending:")) {
            ("holds", _) if checked => None,
            (_, Some(step)) if checked && !step.is_empty() => Some(step.to_string()),
            ("-", _) if !checked => None,
            _ => {
                problems.push(format!(
                    "line {line_no}: state is {state:?}, not {}",
                    if checked {
                        "`holds` or `pending:<step>`"
                    } else {
                        "`-` (the row is not checked)"
                    }
                ));
                continue;
            }
        };
        let row = Row {
            line: line_no,
            pending,
            expect,
        };
        if rows.insert(case.to_string(), row).is_some() {
            problems.push(format!("line {line_no}: a second row for {case}"));
        }
    }
    (rows, problems)
}

/// Every case of `DIRS`, as the manifest names it.
fn manifest_cases() -> BTreeSet<String> {
    let root = super::golden_root();
    let mut paths = Vec::new();
    for dir in DIRS {
        super::collect_cases(&root.join(dir), &mut paths);
    }
    paths
        .iter()
        .map(|p| {
            let rel = p.strip_prefix(&root).expect("a case is under the root");
            let parts: Vec<_> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect();
            parts.join("/")
        })
        .collect()
}

/// Run `silt <cmd> <entry>` on a fresh copy of `case`.
fn run(case: &Case, cmd: &[&str]) -> Output {
    super::run_case(&Case {
        is_dir: case.is_dir,
        source_path: case.source_path.clone(),
        dir: case.dir.clone(),
        file: case.file.clone(),
        expected_base: case.expected_base.clone(),
        directives: Directives {
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            repeat: 1,
            timeout: case.directives.timeout,
            ..Directives::default()
        },
    })
}

/// The codes of the error diagnostics `check --format json` printed.
fn error_codes(out: &Output) -> Result<Vec<String>, String> {
    if out.timed_out {
        return Err("`check` did not exit in time".to_string());
    }
    let diagnostics: Vec<serde_json::Value> = serde_json::from_str(&out.stdout).map_err(|e| {
        format!(
            "`check --format json` printed no diagnostics array ({e}); stdout:\n{}\nstderr:\n{}",
            out.stdout, out.stderr
        )
    })?;
    Ok(diagnostics
        .iter()
        .filter(|d| d["severity"] == "error")
        .map(|d| d["code"].as_str().unwrap_or("?").to_string())
        .collect())
}

/// Why the row's expectation does not hold for `case`; `None` when it
/// holds.
fn violation(case: &Case, expect: &Expect) -> Result<Option<String>, String> {
    let check = run(case, &["check", "--format", "json"]);
    let codes = error_codes(&check)?;
    let found = || {
        if codes.is_empty() {
            "`check` reports no error".to_string()
        } else {
            format!("`check` reports {}", codes.join(", "))
        }
    };
    let before_checking = |id: &str| {
        Code::ALL
            .iter()
            .any(|c| c.id() == id && matches!(c.phase(), Phase::Lex | Phase::Parse))
    };
    Ok(match expect {
        Expect::Reject(code) => {
            if check.code != Some(1) {
                Some(format!("`check` exits with {:?}, not 1", check.code))
            } else if !codes.iter().any(|c| c == code.id()) {
                Some(format!("no {} error: {}", code.id(), found()))
            } else if !before_checking(code.id()) && codes.iter().any(|c| before_checking(c)) {
                // A program the lexer or parser refuses was not checked
                // as written, whatever else is reported for it.
                Some(format!("the program does not parse: {}", found()))
            } else {
                None
            }
        }
        Expect::Accept | Expect::Output => {
            if !codes.is_empty() || check.code != Some(0) {
                Some(format!("{}, exit status {:?}", found(), check.code))
            } else if matches!(expect, Expect::Output) {
                let expected_path = case.expected_base.with_extension("stdout");
                let expected = std::fs::read_to_string(&expected_path)
                    .map_err(|e| format!("cannot read {}: {e}", expected_path.display()))?;
                let ran = run(case, &["run"]);
                let actual = super::portable_paths(&ran.stdout);
                if ran.timed_out || ran.code != Some(0) {
                    Some(format!(
                        "`run` exits with {:?}; stderr:\n{}",
                        ran.code, ran.stderr
                    ))
                } else if actual != expected {
                    Some(format!(
                        "`run` does not print {}:\n--- expected\n{expected}--- actual\n{actual}---",
                        expected_path.display()
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        }
        Expect::Obsolete | Expect::Undetermined => None,
    })
}

/// The rows run as this many tests, each taking every `SHARDS`-th row, so
/// a test runner can spread them over its workers.
const SHARDS: usize = 4;

#[test]
fn soundness_manifest_0() {
    run_shard(0);
}

#[test]
fn soundness_manifest_1() {
    run_shard(1);
}

#[test]
fn soundness_manifest_2() {
    run_shard(2);
}

#[test]
fn soundness_manifest_3() {
    run_shard(3);
}

fn run_shard(shard: usize) {
    let root = super::golden_root();
    let text = std::fs::read_to_string(root.join(MANIFEST)).expect("read the soundness manifest");
    let (rows, mut problems) = parse_manifest(&text);
    let cases = manifest_cases();
    for case in &cases {
        if !rows.contains_key(case) {
            problems.push(format!("{case} has no row"));
        }
    }
    for (case, row) in &rows {
        if !cases.contains(case) {
            problems.push(format!("line {}: {case} is not a case", row.line));
        }
    }
    assert!(
        problems.is_empty(),
        "{MANIFEST} has {} problems:\n  {}",
        problems.len(),
        problems.join("\n  ")
    );

    // The rows are recorded against a build with every cargo feature
    // (some programs name types of feature modules), and the outcome of
    // `check` does not depend on the platform: like the verdict cases,
    // the rows are not run where those are skipped.
    if !super::all_features() {
        eprintln!("soundness manifest not checked: it needs a build with --all-features");
        return;
    }
    if std::env::var_os("SILT_GOLDEN_SKIP_VERDICT").is_some_and(|v| v != "0") {
        eprintln!("soundness manifest not checked: SILT_GOLDEN_SKIP_VERDICT is set");
        return;
    }

    let mut pending: BTreeMap<&str, usize> = BTreeMap::new();
    for row in rows.values() {
        if let Some(step) = &row.pending {
            *pending.entry(step).or_default() += 1;
        }
    }
    if shard == 0 && !pending.is_empty() {
        let steps: Vec<String> = pending
            .iter()
            .map(|(step, n)| format!("{n} for step {step}"))
            .collect();
        eprintln!(
            "soundness manifest: rows still pending: {}",
            steps.join(", ")
        );
    }

    let filter = std::env::var("SILT_GOLDEN_FILTER").ok();
    let checked: Vec<PathBuf> = rows
        .iter()
        .filter(|(_, row)| !matches!(row.expect, Expect::Obsolete | Expect::Undetermined))
        .map(|(case, _)| root.join(case))
        .filter(|path| {
            filter
                .as_ref()
                .is_none_or(|f| path.to_string_lossy().contains(f.as_str()))
        })
        .enumerate()
        .filter(|(i, _)| i % SHARDS == shard)
        .map(|(_, path)| path)
        .collect();
    let by_path: BTreeMap<PathBuf, &Row> = rows
        .iter()
        .map(|(case, row)| (root.join(case), row))
        .collect();
    super::run_cases(&checked, "soundness manifest", |case| {
        let path = if case.is_dir {
            &case.dir
        } else {
            &case.source_path
        };
        let row = by_path[path];
        match (violation(case, &row.expect), &row.pending) {
            (Err(e), _) => vec![format!("(line {}): {e}", row.line)],
            (Ok(Some(why)), None) => vec![format!("(line {}) does not hold: {why}", row.line)],
            (Ok(None), Some(step)) => vec![format!(
                "(line {}) holds now: replace its `pending:{step}` with `holds`",
                row.line
            )],
            (Ok(Some(_)), Some(_)) | (Ok(None), None) => Vec::new(),
        }
    });
}
