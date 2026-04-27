//! Lock test: enforces the integration-test split.
//!
//! Concurrency-dependent tests (`test_channel*`, `test_task*`,
//! `test_deadlock_detected*`, `test_double_close*`, `test_select*`) must live in
//! `tests/integration_concurrency.rs`, NOT `tests/integration.rs`. Cargo runs
//! each `tests/*.rs` as its own process, so isolating these scheduler-poisoning
//! tests in their own binary keeps a panic in any one of them from hanging the
//! other ~600 integration tests.
//!
//! If this test fails, someone moved a concurrency test back into
//! `integration.rs` (or deleted `integration_concurrency.rs`). Move it back
//! out — see commit history for context.

use std::fs;
use std::path::Path;

const FORBIDDEN_PREFIXES: &[&str] = &[
    "fn test_channel",
    "fn test_task",
    "fn test_deadlock_detected",
    "fn test_double_close",
    "fn test_select",
];

fn read_test_file(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

#[test]
fn integration_rs_contains_no_concurrency_tests() {
    let src = read_test_file("tests/integration.rs");
    let mut offenders: Vec<(usize, String)> = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        for prefix in FORBIDDEN_PREFIXES {
            if trimmed.starts_with(prefix) {
                offenders.push((i + 1, line.to_string()));
                break;
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "tests/integration.rs contains concurrency-dependent tests that should \
         live in tests/integration_concurrency.rs instead. Offending lines:\n{}",
        offenders
            .iter()
            .map(|(n, l)| format!("  line {}: {}", n, l))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn integration_concurrency_rs_holds_the_concurrency_tests() {
    let src = read_test_file("tests/integration_concurrency.rs");
    let mut count = 0usize;
    for line in src.lines() {
        let trimmed = line.trim_start();
        for prefix in FORBIDDEN_PREFIXES {
            if trimmed.starts_with(prefix) {
                count += 1;
                break;
            }
        }
    }
    assert!(
        count >= 15,
        "tests/integration_concurrency.rs should contain at least 15 \
         concurrency tests (test_channel*, test_task*, test_deadlock_detected*, \
         test_double_close*, test_select*); found only {count}"
    );
}
