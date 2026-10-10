//! The golden cases as inputs of the oracle: every case under
//! `tests/golden/` (outside the repro corpus) that is run (`cmd: run`,
//! the default) and must end well (`exit: 0`, the default). Its exact
//! `.stdout`, where it has one, is what both runs must write.
//!
//! The cases and their directives are those of `tests/golden/README.md`;
//! `tests/golden/main.rs` is the harness that runs them through the
//! binary.

use std::path::{Path, PathBuf};

use crate::oracle::{Expect, Input, Source};
use crate::sweep::{conclude, name_of, repo_root, run, sample, skips};

/// The directory of the imported repro corpus, whose cases say nothing
/// of how they run: they are inputs of their own (`corpora.rs`).
pub const REPROS: &str = "tests/golden/repros";

/// Every case under `root`: each `.silt` file outside a case directory,
/// and each directory that holds a `main.silt` or is a package.
pub fn collect_cases(root: &Path, out: &mut Vec<PathBuf>) {
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
/// and no `main.silt` of its own.
fn is_package_case(dir: &Path) -> bool {
    !dir.join("main.silt").is_file()
        && dir.join("silt.toml").is_file()
        && dir.join("src/main.silt").is_file()
}

/// Whether the cargo feature a case names is enabled: the oracle's
/// binary is built with the features of the `silt` it would run.
fn feature_enabled(name: &str) -> bool {
    let features = [
        ("repl", cfg!(feature = "repl")),
        ("lsp", cfg!(feature = "lsp")),
        ("watch", cfg!(feature = "watch")),
        ("local-clock", cfg!(feature = "local-clock")),
        ("http", cfg!(feature = "http")),
        ("tcp", cfg!(feature = "tcp")),
        ("tcp-tls", cfg!(feature = "tcp-tls")),
        ("postgres", cfg!(feature = "postgres")),
        ("postgres-tls", cfg!(feature = "postgres-tls")),
        ("debug-build", cfg!(debug_assertions)),
    ];
    features.contains(&(name, true))
}

/// Whether the directives of a case's `source` (its leading `-- key:
/// value` comment lines) say that it is run, ends with status 0, and is
/// a case of this build.
fn runs_and_succeeds(source: &str) -> bool {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut qualifies = true;
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
        let value = value.trim();
        qualifies &= match key.trim() {
            "cmd" => value == "run",
            "exit" => value == "0",
            "requires-feature" => feature_enabled(value),
            "without-feature" => !feature_enabled(value),
            _ => true,
        };
    }
    qualifies
}

/// The entry file of `case`, its text, and where its expected stdout
/// is; `None` for a case whose entry file is not text.
fn entry_of(case: &Path) -> Option<(PathBuf, String, PathBuf)> {
    let (entry, expected) = if !case.is_dir() {
        (case.to_path_buf(), case.with_extension("stdout"))
    } else if is_package_case(case) {
        (case.join("src/main.silt"), case.join("case.stdout"))
    } else {
        (case.join("main.silt"), case.join("case.stdout"))
    };
    let text = std::fs::read_to_string(&entry).ok()?;
    Some((entry, text, expected))
}

/// The files of `case`, whose entry file `entry` has the text `text`,
/// as the golden harness lays them out for the binary.
fn source_of(case: &Path, entry: PathBuf, text: String) -> Source {
    if !case.is_dir() {
        // A single file is run in a directory of its own.
        let file = entry.file_name().expect("a file").to_string_lossy();
        Source::Memory(vec![(file.into_owned(), text)])
    } else if case.join("silt.toml").is_file() {
        // A package, whose entry is `main.silt` or `src/main.silt`: its
        // manifest and lock file are read where they are.
        Source::Package(entry)
    } else {
        Source::Script(entry)
    }
}

/// The case `case` as an input of which nothing is expected; `None`
/// for a case whose entry file is not text.
pub fn plain_input(case: &Path) -> Option<Input> {
    let (entry, text, _) = entry_of(case)?;
    Some(Input {
        name: name_of(case),
        source: source_of(case, entry, text),
        real_time: false,
        expect: Expect::default(),
    })
}

/// The golden cases that qualify, sorted by name.
fn inputs() -> Vec<Input> {
    let mut cases = Vec::new();
    collect_cases(&repo_root().join("tests/golden"), &mut cases);
    let mut inputs = Vec::new();
    for case in cases {
        let name = name_of(&case);
        if name.starts_with(REPROS) {
            continue;
        }
        let Some((entry, text, expected)) = entry_of(&case) else {
            continue;
        };
        if !runs_and_succeeds(&text) {
            continue;
        }
        inputs.push(Input {
            name,
            source: source_of(&case, entry, text),
            real_time: true,
            expect: Expect {
                succeeds: true,
                stdout: std::fs::read_to_string(expected).ok(),
                end: None,
            },
        });
    }
    inputs
}

#[test]
fn golden_cases_that_run_and_succeed() {
    let skips = skips();
    let all = inputs();
    assert!(all.len() > 1000, "only {} golden cases qualify", all.len());
    for skip in skips
        .iter()
        .filter(|skip| skip.input.starts_with("tests/golden/") && !skip.input.starts_with(REPROS))
    {
        assert!(
            all.iter().any(|input| input.name == skip.input),
            "tests/oracle/skip.txt names {}, which is no golden case the oracle runs",
            skip.input
        );
    }
    let inputs = sample(all, 1, &skips);
    let verdicts = run(&inputs, &skips);
    conclude("golden cases", &inputs, &verdicts, &skips);
}
