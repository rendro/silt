//! The formatter's property runner on a sample: every example as it is,
//! and with a comment in every seventh gap between its tokens. The whole
//! sweep is in the `heavy` suite (`tests/heavy/fmt_property_sweep_tests.rs`).

use crate::fmt_property::{SUITE_WORKERS, conclude, examples, mutants, plain, run};

#[test]
fn examples_and_a_sample_of_their_comment_mutants() {
    let examples = examples();
    let mut jobs = plain(&examples);
    jobs.extend(mutants(&examples, 7));
    let report = run(&jobs, SUITE_WORKERS);
    conclude("sample", &report);
}
