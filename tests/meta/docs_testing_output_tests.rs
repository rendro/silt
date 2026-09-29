//! Regression lock for the `silt test` example output shown in
//! `docs/language/testing.md`.
//!
//! Round 60 audit G2-docs: the docs previously claimed the test-runner
//! prints `PASS math_test.silt::test_addition`. The actual CLI prints
//! `PASS ./math_test.silt::test_addition` whenever tests are discovered
//! from a directory (the default — `silt test` with no file arg). The
//! `./` prefix is only dropped when a file is passed directly. The fix
//! documents the discovery-mode form (which is what the page's "Running
//! this file produces:" narration implies) and leaves a note for the
//! single-file form.
//!
//! This test is a source-grep on the doc file so it cannot drift back
//! to the old stripped form, and it also pins the single-file note so
//! nobody quietly re-introduces the contradiction.

use std::path::Path;

fn read_testing_doc() -> String {
    let path = Path::new("docs/language/testing.md");
    std::fs::read_to_string(path).expect("docs/language/testing.md must be readable")
}

/// The corrected `PASS ./math_test.silt::test_addition` shape must be
/// present for all four shown tests/skips.
#[test]
fn testing_doc_shows_dotted_prefix_for_discovery_mode() {
    let doc = read_testing_doc();
    for expected in [
        "PASS ./math_test.silt::test_addition",
        "PASS ./math_test.silt::test_string_length",
        "SKIP ./math_test.silt::skip_test_not_ready_yet",
        "PASS ./math_test.silt::test_with_helper",
    ] {
        assert!(
            doc.contains(expected),
            "docs/language/testing.md must show `{expected}` in the \
             example output block — that's the actual shape the CLI \
             prints when tests are discovered from a directory. Found \
             doc:\n{doc}"
        );
    }
}

/// The stale stripped-prefix forms must not reappear in the example
/// output block. Matching on the line-start form prevents an accidental
/// match inside surrounding prose.
#[test]
fn testing_doc_drops_stripped_prefix_in_example_output() {
    let doc = read_testing_doc();
    for stale in [
        "  PASS math_test.silt::test_addition\n",
        "  PASS math_test.silt::test_string_length\n",
        "  SKIP math_test.silt::skip_test_not_ready_yet\n",
        "  PASS math_test.silt::test_with_helper\n",
    ] {
        assert!(
            !doc.contains(stale),
            "docs/language/testing.md still shows the old stripped-prefix \
             form `{}` in the example output block. The CLI actually \
             prints `  PASS ./math_test.silt::...` under discovery mode. \
             Use the dotted form.",
            stale.trim_end()
        );
    }
}

/// The single-file-form note must be present so the two shapes are
/// explicitly reconciled (otherwise a reader who typed `silt test
/// math_test.silt` and saw no `./` thinks the docs are stale again).
#[test]
fn testing_doc_explains_single_file_form_drops_prefix() {
    let doc = read_testing_doc();
    let lower = doc.to_lowercase();
    assert!(
        lower.contains("single file")
            || lower.contains("single-file")
            || lower.contains("passed directly"),
        "docs/language/testing.md must reconcile the discovery-mode \
         `./file.silt` prefix with the single-file invocation that \
         drops it. Got doc:\n{doc}"
    );
    assert!(
        doc.contains("PASS math_test.silt::test_addition"),
        "docs/language/testing.md must show the single-file form \
         `PASS math_test.silt::test_addition` (no `./`) so both shapes \
         are documented."
    );
}
