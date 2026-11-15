//! The nightly runs of Miri and the sanitizers name the tests they run
//! by filters (`value::thin`, `runtime::handle`) and the tests they
//! leave out by `--skip` (.github/scripts/miri.sh, sanitizer.sh). A
//! filter that names a module no longer there runs no test, and libtest
//! passes a run of none: the run would go on green and check nothing.
//! So every filter must name a module of the crate that has tests, and
//! every skipped name must be part of a test's name. And the nightly
//! workflow runs both scripts.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let path = repo().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The lines of a shell script that are not comments.
fn code(script: &str) -> Vec<String> {
    script
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The filters (`a::b` or `a::`) a script's code names.
fn filters(lines: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for line in lines {
        for word in line.split(|c: char| c.is_whitespace() || "()\"'".contains(c)) {
            if word.contains("::")
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c == ':')
            {
                found.push(word.to_string());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// The names after `--skip` in a script's code.
fn skipped(lines: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for line in lines {
        let mut words = line.split_whitespace();
        while let Some(word) = words.next() {
            if word == "--skip" {
                found.push(words.next().expect("a name after --skip").to_string());
            }
        }
    }
    found
}

/// The source file of the module `path` (`value::thin`), if it has one.
fn module_file(path: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = path.split("::").filter(|p| !p.is_empty()).collect();
    let base = parts
        .iter()
        .fold(repo().join("src"), |dir, part| dir.join(part));
    [base.with_extension("rs"), base.join("mod.rs")]
        .into_iter()
        .find(|p| p.is_file())
}

/// Every `.rs` file under `dir`.
fn sources(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            sources(&path, into);
        } else if path.extension().is_some_and(|e| e == "rs") {
            into.push(path);
        }
    }
}

/// Whether a file of the module `file` (it, or one below its
/// directory) holds a test.
fn has_tests(file: &Path) -> bool {
    let dir = if file.file_name().is_some_and(|n| n == "mod.rs") {
        file.parent().expect("a directory").to_path_buf()
    } else {
        file.with_extension("")
    };
    let mut files = vec![file.to_path_buf()];
    if dir.is_dir() {
        sources(&dir, &mut files);
    }
    files
        .iter()
        .any(|f| std::fs::read_to_string(f).is_ok_and(|t| t.contains("#[test]")))
}

#[test]
fn the_tool_runs_name_tests_that_exist() {
    let mut all = Vec::new();
    sources(&repo().join("src"), &mut all);
    let test_names: Vec<String> = all
        .iter()
        .flat_map(|f| {
            let text = std::fs::read_to_string(f).unwrap_or_default();
            text.lines()
                .filter_map(|l| l.trim().strip_prefix("fn ").map(str::to_string))
                .collect::<Vec<_>>()
        })
        .collect();
    for script in [".github/scripts/miri.sh", ".github/scripts/sanitizer.sh"] {
        let lines = code(&read(script));
        let filters = filters(&lines);
        assert!(!filters.is_empty(), "{script} names no tests");
        for filter in filters {
            let file = module_file(&filter)
                .unwrap_or_else(|| panic!("{script}: `{filter}` is no module of the crate"));
            assert!(has_tests(&file), "{script}: `{filter}` has no tests");
        }
        for name in skipped(&lines) {
            if name.contains("::") {
                continue; // a module, checked above
            }
            assert!(
                test_names.iter().any(|f| f.contains(&name)),
                "{script}: `--skip {name}` is part of no function's name"
            );
        }
    }
}

#[test]
fn the_nightly_workflow_runs_miri_and_both_sanitizers() {
    let nightly = read(".github/workflows/fuzz-nightly.yml");
    assert!(nightly.contains("run: bash .github/scripts/miri.sh"));
    assert!(nightly.contains("run: bash .github/scripts/sanitizer.sh ${{ matrix.sanitizer }}"));
    assert!(nightly.contains("sanitizer: [thread, address]"));
}
