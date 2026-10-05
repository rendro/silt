use super::*;

pub(super) fn check_errors(input: &str) -> Vec<Diagnostic> {
    crate::session::testing::analyze_str(input).1
}

pub(super) fn assert_no_errors(input: &str) {
    let errors = check_errors(input);
    let hard: Vec<_> = errors
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        hard.is_empty(),
        "expected no type errors, got:\n{}",
        hard.iter()
            .map(|e| format!("  {}", e.message))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

pub(super) fn assert_has_error(input: &str, expected: &str) {
    let errors = check_errors(input);
    assert!(
        errors.iter().any(|e| e.message.contains(expected)),
        "expected error containing '{expected}', got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}
