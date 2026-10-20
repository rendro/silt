//! Claims the docs made that the language does not keep (audit of
//! 2026-10-19). Each one is a phrase that may not come back: the docs
//! and README are read with their line breaks and runs of spaces made
//! one space, so a claim is found however its prose is wrapped.

use std::path::{Path, PathBuf};

/// `README.md` and every `.md` under `docs/`, each with its text
/// in one line.
fn docs() -> Vec<(PathBuf, String)> {
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = vec![root.join("README.md")];
    collect(&root.join("docs"), &mut paths);
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (path, flat)
        })
        .collect()
}

fn assert_no_doc_says(claims: &[&str], why: &str) {
    let found: Vec<String> = docs()
        .iter()
        .flat_map(|(path, text)| {
            claims
                .iter()
                .filter(|claim| text.contains(*claim))
                .map(move |claim| format!("{}: {claim:?}", path.display()))
        })
        .collect();
    assert!(found.is_empty(), "{why}:\n{}", found.join("\n"));
}

/// silt has no float recovery operator: `1.0 / 0.0 else 0.0` does not
/// parse, and a division by zero is a runtime error.
#[test]
fn no_doc_lists_a_float_recovery_operator() {
    assert_no_doc_says(
        &["float recovery"],
        "a doc names an operator silt does not have",
    );
}

/// A binding named like a builtin module hides it in its scope, as any
/// binding hides an import of that name, and no warning is given.
#[test]
fn no_doc_promises_a_warning_for_shadowing_a_builtin_module() {
    assert_no_doc_says(
        &["emits a warning when a binding shadows"],
        "a doc promises a warning silt does not give",
    );
}

/// `channel.new` takes the capacity; `channel.new()` is a type error.
#[test]
fn no_doc_calls_channel_new_without_a_capacity() {
    assert_no_doc_says(
        &[
            "channel.new()",
            "takes zero or one argument",
            "with no arguments creates a true rendezvous channel",
        ],
        "a doc says channel.new can be called without a capacity",
    );
}

/// The failure `task.join` raises points at where the task failed, and
/// its call stack goes on with the calls that led to the join: it is
/// not reported at the join.
#[test]
fn no_doc_places_a_joined_failure_at_the_join() {
    assert_no_doc_says(
        &["`joined task failed: <msg>` at the call site"],
        "a doc says a joined failure is reported at the join",
    );
}
