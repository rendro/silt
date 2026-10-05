//! The formatter's property runner (`tests/frontend/fmt_property/mod.rs`)
//! on everything: the examples, the docs' snippets, the golden cases,
//! the formatter's fuzz corpus and the directory `SILT_FMT_CORPUS` names,
//! each as it is; and the comment mutants of the examples, the snippets
//! and the golden cases.
//!
//! By default the mutants are a sample: a comment in every n-th gap of
//! the doc snippets and of the golden cases outside `repros/` (the
//! `frontend` suite has the sample of the examples). `SILT_FMT_FULL=1`
//! puts one in every gap of every example, snippet and golden case
//! (800,000 inputs; run it in release):
//!
//! ```text
//! SILT_FMT_FULL=1 SILT_FMT_REPORT=/tmp/fmt.txt cargo test --release --all-features --test heavy every_input -- --nocapture
//! ```

use crate::fmt_property::{
    SUITE_WORKERS, conclude, doc_snippets, examples, extra_corpus, fuzz_corpus, golden_files,
    mutants, plain, run, stress,
};

/// Without `SILT_FMT_FULL`, every n-th gap of a doc snippet and of a
/// golden case gets a comment.
const DOC_GAPS: usize = 3;
const GOLDEN_GAPS: usize = 41;

#[test]
fn every_input_and_its_comment_mutants() {
    let full = std::env::var_os("SILT_FMT_FULL").is_some_and(|v| v != "0");
    let examples = examples();
    let docs = doc_snippets();
    let (repros, golden): (Vec<_>, Vec<_>) = golden_files()
        .into_iter()
        .partition(|input| input.name.starts_with("tests/golden/repros/"));
    let fuzz = fuzz_corpus();
    let extra = extra_corpus();
    let mut jobs = Vec::new();
    for inputs in [&examples, &docs, &golden, &repros, &fuzz, &extra] {
        jobs.extend(plain(inputs));
    }
    if full {
        for inputs in [&examples, &docs, &golden, &repros] {
            jobs.extend(mutants(inputs, 1));
        }
    } else {
        jobs.extend(mutants(&docs, DOC_GAPS));
        jobs.extend(mutants(&golden, GOLDEN_GAPS));
    }
    // The full sweep is run on its own and takes every CPU, unless
    // `SILT_FMT_WORKERS` says how many threads to use.
    let workers = match std::env::var("SILT_FMT_WORKERS") {
        Ok(n) => n.parse().expect("SILT_FMT_WORKERS is a number"),
        Err(_) if full => std::thread::available_parallelism().map_or(4, |n| n.get()),
        Err(_) => SUITE_WORKERS,
    };
    let report = run(&jobs, workers);
    conclude(if full { "full sweep" } else { "sweep" }, &report);
}

/// Inputs with several comments each, at random sites: what one comment
/// per input does not find. Off unless `SILT_FMT_STRESS=<count>` says
/// how many; `SILT_FMT_SEED=<n>` draws other ones.
///
/// ```text
/// SILT_FMT_STRESS=200000 cargo test --release --all-features --test heavy random_comments -- --nocapture
/// ```
#[test]
fn random_comments() {
    let Ok(count) = std::env::var("SILT_FMT_STRESS") else {
        eprintln!("skipped: set SILT_FMT_STRESS=<count> to run it");
        return;
    };
    let count: usize = count.parse().expect("SILT_FMT_STRESS is a number");
    let seed: u64 = match std::env::var("SILT_FMT_SEED") {
        Ok(seed) => seed.parse().expect("SILT_FMT_SEED is a number"),
        Err(_) => 1,
    };
    let mut inputs = examples();
    inputs.extend(doc_snippets());
    inputs.extend(
        golden_files()
            .into_iter()
            .filter(|input| !input.name.starts_with("tests/golden/repros/")),
    );
    let jobs = stress(&inputs, count, seed);
    let workers = match std::env::var("SILT_FMT_WORKERS") {
        Ok(n) => n.parse().expect("SILT_FMT_WORKERS is a number"),
        Err(_) => std::thread::available_parallelism().map_or(4, |n| n.get()),
    };
    let report = run(&jobs, workers);
    conclude("random comments", &report);
}
