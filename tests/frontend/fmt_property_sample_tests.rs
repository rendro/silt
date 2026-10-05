//! The formatter's property runner on a sample: every example as it is,
//! and with a comment in every fourteenth gap between its tokens. The whole
//! sweep is in the `heavy` suite (`tests/heavy/fmt_property_sweep_tests.rs`).

use crate::fmt_property::{
    Expect, SUITE_WORKERS, conclude, current_formatter, examples, mutants, plain, run,
};

/// `silt fmt` refuses some of these inputs today (it would lose the
/// comment), so the run is marked as known to fail. Stage 8 step A3
/// switches `silt fmt` to the printer that passes all of them and
/// changes the mark to `Expect::Clean`.
#[test]
fn examples_and_a_sample_of_their_comment_mutants() {
    let examples = examples();
    let mut jobs = plain(&examples);
    jobs.extend(mutants(&examples, 14));
    let report = run(&jobs, current_formatter, SUITE_WORKERS);
    conclude("sample", &report, Expect::KnownFailing);
}
