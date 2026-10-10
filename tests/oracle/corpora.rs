//! The corpora as inputs of the oracle: the seeds of the fuzz targets
//! that read silt source (`fuzz/corpus/`), the examples (`examples/`),
//! and the imported repro corpus (`tests/golden/repros/`). Nothing is
//! known of how these programs run, and most are not run at all: a
//! seed need not check clean or have a `main`, and many examples read
//! files or the network. What is run must agree with itself.
//!
//! A text that several corpora hold (the examples seed every fuzz
//! target) is one input, under the first of its names.

use std::collections::HashSet;
use std::path::Path;

use crate::goldens::{REPROS, collect_cases, plain_input};
use crate::oracle::{Expect, Input, Source};
use crate::sweep::{conclude, full, name_of, repo_root, run, sample, skips};

/// The `.silt` files of the directory `dir`, sorted, each an input of
/// its own.
fn files_of(dir: &Path, inputs: &mut Vec<Input>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    for file in files {
        if file.extension().is_none_or(|e| e != "silt") {
            continue;
        }
        // A seed that is not text is the lexer's target's.
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        inputs.push(Input {
            name: name_of(&file),
            source: Source::Memory(vec![("main.silt".to_string(), text)]),
            expect: Expect::default(),
        });
    }
}

/// `inputs` without those whose text an earlier one has.
fn distinct(inputs: Vec<Input>) -> Vec<Input> {
    let mut seen = HashSet::new();
    inputs
        .into_iter()
        .filter(|input| match &input.source {
            Source::Memory(files) => seen.insert(files[0].1.clone()),
            _ => true,
        })
        .collect()
}

/// The skip entries under `prefix` name inputs of `all`.
fn check_skips(all: &[Input], prefixes: &[&str]) {
    for skip in skips() {
        if prefixes.iter().any(|prefix| skip.input.starts_with(prefix)) {
            assert!(
                all.iter().any(|input| input.name == skip.input),
                "tests/oracle/skip.txt names {}, which is no input of the oracle",
                skip.input
            );
        }
    }
}

#[test]
fn fuzz_corpora_and_examples() {
    let root = repo_root();
    let mut all = Vec::new();
    // The compiler's and the typechecker's seeds first: theirs are the
    // names of what the other corpora share with them.
    let mut corpora = vec!["fuzz_compiler".to_string(), "fuzz_typechecker".to_string()];
    let listed = std::fs::read_dir(root.join("fuzz/corpus")).expect("fuzz/corpus");
    let mut others: Vec<String> = listed
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !corpora.contains(name))
        .collect();
    others.sort();
    corpora.extend(others);
    for corpus in &corpora {
        files_of(&root.join("fuzz/corpus").join(corpus), &mut all);
    }
    files_of(&root.join("examples"), &mut all);
    let all = distinct(all);
    assert!(all.len() > 80, "only {} corpus texts", all.len());
    check_skips(&all, &["fuzz/corpus/", "examples/"]);
    let skips = skips();
    let inputs = sample(all, 1, &skips);
    let verdicts = run(&inputs, &skips);
    conclude("fuzz corpora and examples", &inputs, &verdicts, &skips);
}

/// Only in a full sweep, until the oracle has a step budget: the
/// corpus has programs that do not end, and each costs two runs of the
/// watchdog's 60 s.
#[test]
fn repro_corpus() {
    if !full() {
        eprintln!("skipped: set SILT_ORACLE_FULL=1 to run it");
        return;
    }
    let mut cases = Vec::new();
    collect_cases(&repo_root().join(REPROS), &mut cases);
    let all: Vec<Input> = cases.iter().filter_map(|case| plain_input(case)).collect();
    assert!(all.len() > 1000, "only {} repros", all.len());
    check_skips(&all, &[REPROS]);
    let skips = skips();
    let inputs = sample(all, 1, &skips);
    let verdicts = run(&inputs, &skips);
    conclude("repro corpus", &inputs, &verdicts, &skips);
}
