//! The formatter's property runner on a sample: every example as it is,
//! and with a comment in every fourteenth gap between its tokens. The whole
//! sweep is in the `heavy` suite (`tests/heavy/fmt_property_sweep_tests.rs`).

use crate::fmt_property::{
    Expect, Formatter, SUITE_WORKERS, conclude, current_formatter, examples, mutants,
    next_formatter, plain, run,
};

fn sample(what: &str, format: Formatter, expect: Expect) {
    let examples = examples();
    let mut jobs = plain(&examples);
    jobs.extend(mutants(&examples, 14));
    let report = run(&jobs, format, SUITE_WORKERS);
    conclude(what, &report, expect);
}

/// `silt fmt` refuses some of these inputs today (it would lose the
/// comment), so the run is marked as known to fail. Stage 8 step A3
/// switches `silt fmt` to the printer that passes all of them and
/// changes the mark to `Expect::Clean`.
#[test]
fn examples_and_a_sample_of_their_comment_mutants() {
    sample("sample", current_formatter, Expect::KnownFailing);
}

/// The printer of `src/format/` passes all of them.
#[test]
fn new_printer_on_examples_and_a_sample_of_their_comment_mutants() {
    sample("sample, new printer", next_formatter, Expect::Clean);
}
