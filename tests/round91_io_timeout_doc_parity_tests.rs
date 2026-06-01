//! Round 91 lock: docs/concurrency.md must not advertise that SILT_IO_TIMEOUT
//! accepts a bare integer (milliseconds). The runtime parser
//! `src/scheduler.rs::parse_duration` REJECTS bare integers (no unit char ->
//! `None` -> watchdog silently disabled), and a locked unit test asserts
//! `parse_duration("30") == None`. The docs previously claimed otherwise on two
//! sites (the `=5000` example and the "bare integer (milliseconds)" clause).
//!
//! This is a source-grep lock against the doc's user-facing text — the correct
//! lock shape for a documentation/user-string fix.

const CONCURRENCY_DOC: &str = include_str!("../docs/concurrency.md");

#[test]
fn io_timeout_doc_drops_misleading_bare_integer_claim() {
    // The misleading positive example must be gone: docs no longer tell users to
    // "Set SILT_IO_TIMEOUT=5000" as if a bare ms integer works.
    assert!(
        !CONCURRENCY_DOC.contains("Setting `SILT_IO_TIMEOUT=5000`"),
        "docs/concurrency.md still advertises `SILT_IO_TIMEOUT=5000` as a working \
         value, but parse_duration rejects bare integers (watchdog stays disabled)"
    );

    // The misleading accepted-formats clause must be gone.
    assert!(
        !CONCURRENCY_DOC.contains("bare integer (milliseconds)"),
        "docs/concurrency.md still claims a bare integer (milliseconds) is an \
         accepted SILT_IO_TIMEOUT format, but parse_duration requires a unit suffix"
    );
}

#[test]
fn io_timeout_doc_states_suffixed_only_and_warns_on_bare_integer() {
    // The doc must document the accepted suffixed forms.
    for form in ["`5s`", "`500ms`", "`2m`", "`1h`"] {
        assert!(
            CONCURRENCY_DOC.contains(form),
            "docs/concurrency.md no longer documents the accepted suffixed form {form}"
        );
    }

    // The doc must warn that a bare integer is NOT accepted.
    assert!(
        CONCURRENCY_DOC.contains("is NOT accepted"),
        "docs/concurrency.md must explicitly state a bare integer is NOT accepted"
    );
}
