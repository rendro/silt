//! Architectural lock: canonical type names are the single source of truth.
//!
//! The string `"Range"` must not appear in `src/compiler/mod.rs` or
//! `src/vm/mod.rs` outside of:
//!
//! - Comments (`//` and `///`) — documentation may freely mention Range.
//! - `src/types/canonical.rs` — the canonicaliser itself owns the
//!   reduction rule and references the surface name explicitly. (Not
//!   scanned by these tests; lives outside the dispatch path.)
//! - Representation-level / debug paths — e.g. an internal `type_name`
//!   helper that names every `Value` variant for invariant / arithmetic
//!   diagnostics. Those uses preserve user-facing display fidelity
//!   (a Range value in a `1 + 1..5` error should print "Range", not
//!   "List"). To stay outside this lock the helper uses
//!   `stringify!(Range)` instead of the bare `"Range"` literal.
//!
//! These tests catch future regressions where someone reintroduces a
//! Range special-case at the dispatch-key layer that drifts from the
//! canonical name. They are paired with the unit-level tests in
//! `src/types/canonical.rs` (`canonicalize_type_name_collapses_range_to_list`,
//! `dispatch_name_for_value_range_returns_list`) and the runtime tests
//! in `tests/vm_range_receiver_trait_method_tests.rs`.

/// Strip line comments before scanning. Both `//` and `///` start with
/// `//`, so a single `find("//")` covers doc-comments and ordinary
/// comments. Block comments (`/* ... */`) are not used in the silt
/// codebase, so we don't need to handle them.
fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

/// Find every line in `src` whose code portion (comments stripped)
/// contains the literal `"Range"` string. Returns `(line_number,
/// line_text)` tuples for assertion error messages.
fn lines_containing_range_literal(src: &str) -> Vec<(usize, String)> {
    src.lines()
        .enumerate()
        .filter(|(_, line)| strip_comment(line).contains("\"Range\""))
        .map(|(i, l)| (i + 1, l.to_string()))
        .collect()
}

#[test]
fn no_range_string_in_compiler() {
    let src = include_str!("../src/compiler/mod.rs");
    let bad = lines_containing_range_literal(src);
    assert!(
        bad.is_empty(),
        "src/compiler/mod.rs contains the literal \"Range\" string in code (not comments). \
         Use canonical_name() / canonicalize_type_name() from src/types/canonical.rs instead.\n\
         Offending lines:\n{:#?}",
        bad
    );
}

#[test]
fn no_range_string_in_vm_mod() {
    let src = include_str!("../src/vm/mod.rs");
    let bad = lines_containing_range_literal(src);
    assert!(
        bad.is_empty(),
        "src/vm/mod.rs contains the literal \"Range\" string in code (not comments). \
         Use canonical_name() / dispatch_name_for_value() from src/types/canonical.rs \
         instead. If the use is representation-level (debug / arithmetic-error helper \
         like `type_name`), use `stringify!(Range)` so this lock can see it as \
         non-dispatch code.\nOffending lines:\n{:#?}",
        bad
    );
}

// ── Sanity: the helper actually catches regressions ────────────────

#[test]
fn strip_comment_removes_line_comments() {
    assert_eq!(strip_comment(r#"let x = "Range"; // and a comment"#), r#"let x = "Range"; "#);
    assert_eq!(strip_comment(r#"// only a comment with "Range""#), "");
    assert_eq!(strip_comment(r#"/// doc-comment with "Range""#), "");
    assert_eq!(strip_comment(r#"no comment at all"#), r#"no comment at all"#);
}

#[test]
fn lines_containing_range_literal_finds_dispatch_keys() {
    // Synthetic input: the helper must catch a literal "Range" in
    // code, ignore it in comments, and ignore code that doesn't
    // mention Range at all.
    let src = "fn dispatch() -> &'static str { \"Range\" }\n\
               // this comment mentions \"Range\" — should be ignored\n\
               let x = \"List\"; // benign\n";
    let hits = lines_containing_range_literal(src);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, 1);
}
