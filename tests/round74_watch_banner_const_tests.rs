//! Round 74 lock test for the `WATCH_BANNER` extraction.
//!
//! Background: `src/watch.rs` printed the verbatim string
//! `"\n[watch] Watching for changes..."` from three different
//! `eprintln!` call sites — initial-run, debounced-rerun, and the
//! immediate-rerun branch. Future edits to the wording risked
//! diverging the three sites silently.
//!
//! Round 74 hoisted the banner into `const WATCH_BANNER: &str`. These
//! tests pin the extraction:
//!
//! 1. The constant `WATCH_BANNER` must be present in the source
//!    (proves the const exists).
//! 2. The verbatim banner literal `"\n[watch] Watching for changes..."`
//!    must appear AT MOST once in the source (proves the three call
//!    sites no longer carry their own copies).

#[test]
fn watch_banner_constant_exists_and_is_unique() {
    let src = include_str!("../src/watch.rs");

    // (1) The constant name must be present.
    assert!(
        src.contains("WATCH_BANNER"),
        "src/watch.rs must define a `WATCH_BANNER` constant (round 74 \
         dedup of the three duplicate `eprintln!` call sites). If you \
         renamed it, update this test to match."
    );

    // (2) The verbatim banner literal must appear at most once.
    // Match the EXACT byte sequence that was previously duplicated:
    //     "\n[watch] Watching for changes..."
    // — including the leading `\n` escape and trailing `...`.
    let needle = "\"\\n[watch] Watching for changes...\"";
    let count = src.matches(needle).count();
    assert!(
        count <= 1,
        "the verbatim banner literal {needle:?} must appear at most \
         once in src/watch.rs after the round-74 extraction; found \
         {count} occurrences. Did someone re-inline the banner?"
    );
}
