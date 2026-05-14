//! Round-86 L6 doc-parity lock: the typechecker-side mirror of
//! `canonicalize_type_name` in `src/typechecker/mod.rs` carries a
//! doc-comment that must enumerate the same collapses as the
//! authoritative implementation in
//! `src/types/canonical.rs::canonicalize_type_name`.
//!
//! Pre-fix the typechecker doc-comment claimed "only Range→List" even
//! though the body handled Range / Fun / Unit / user-alias chains.
//! Stale doc-comments mislead future readers and audit reviewers; the
//! parity-lock below grep-scans both doc-comments and asserts they
//! mention each load-bearing collapse term (`Range`, `Fun`, `Unit` /
//! `()`, and "alias").
//!
//! Strategy:
//!   * `include_str!` both files.
//!   * Locate the doc-comment immediately above each function
//!     definition (lines starting with `///` up to the `pub fn`
//!     signature).
//!   * Assert that every collapse term in the authoritative doc /
//!     impl body also appears in the typechecker-side doc.

const TYPECHECKER_SRC: &str = include_str!("../src/typechecker/mod.rs");
const CANONICAL_SRC: &str = include_str!("../src/types/canonical.rs");

/// Extract the doc-comment block immediately preceding the
/// `pub(super) fn canonicalize_type_name(` or
/// `pub fn canonicalize_type_name(` signature in `src`.
///
/// Returns the contiguous run of `///` lines (with their leading
/// whitespace) directly above the signature. Lines are joined with
/// `\n` for substring inspection.
fn extract_doc_above_fn(src: &str, signature_needle: &str) -> String {
    let fn_idx = src
        .find(signature_needle)
        .unwrap_or_else(|| panic!("could not find `{signature_needle}` in source"));
    // Walk backwards line-by-line from `fn_idx`, collecting `///`
    // lines until we hit a non-doc line.
    let before = &src[..fn_idx];
    let mut lines: Vec<&str> = before.lines().collect();
    let mut doc_lines: Vec<&str> = Vec::new();
    while let Some(line) = lines.pop() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("///") {
            doc_lines.push(line);
        } else if trimmed.is_empty() {
            // Allow no blank lines between doc and fn; bail on any
            // non-doc, non-blank line.
            break;
        } else {
            break;
        }
    }
    doc_lines.reverse();
    doc_lines.join("\n")
}

#[test]
fn typechecker_doc_enumerates_every_authoritative_collapse() {
    let typechecker_doc = extract_doc_above_fn(
        TYPECHECKER_SRC,
        "pub(super) fn canonicalize_type_name(",
    );
    assert!(
        !typechecker_doc.is_empty(),
        "typechecker-side `canonicalize_type_name` has no doc-comment — \
         expected one enumerating the Range/Fun/Unit/alias collapses."
    );

    // Each collapse term that MUST appear in the typechecker-side
    // doc. These mirror the rules implemented in
    // `src/types/canonical.rs::canonicalize_type_name` and the
    // typechecker's own function body.
    let required_terms = [
        // Range → List collapse (Phase B).
        "Range",
        // Fun → Fn collapse (Round 71 follow-up).
        "Fun",
        // () / Unit collapse (Round 75 TYPE-3).
        // Either spelling is sufficient — the doc mentions both, but
        // we assert "Unit" because it's the canonical side and the
        // typechecker doc should always mention it.
        "Unit",
        // Phase D user-declared aliases.
        "alias",
    ];

    let mut missing: Vec<&str> = Vec::new();
    for term in &required_terms {
        if !typechecker_doc.contains(term) {
            missing.push(term);
        }
    }
    assert!(
        missing.is_empty(),
        "typechecker-side `canonicalize_type_name` doc-comment is missing \
         the following collapse term(s): {missing:?}. The doc must \
         enumerate every collapse handled in the body (and that the \
         authoritative mirror in `src/types/canonical.rs::canonicalize_type_name` \
         also handles): {required_terms:?}.\n\nCurrent doc:\n{typechecker_doc}"
    );
}

#[test]
fn authoritative_doc_or_body_mentions_each_collapse_term() {
    // Sanity check that the authoritative implementation in
    // `src/types/canonical.rs` itself names every collapse term —
    // either in the doc-comment or in the function body. If the
    // authoritative source ever loses one of these terms (e.g.
    // because a collapse rule was removed), this test fires and
    // forces a coordinated update across both copies.
    let canonical_doc = extract_doc_above_fn(
        CANONICAL_SRC,
        "pub fn canonicalize_type_name(",
    );
    // Also grab the function body so the assertion is satisfied
    // whether a term shows up in the doc or in the implementation.
    let body_start = CANONICAL_SRC
        .find("pub fn canonicalize_type_name(")
        .expect("canonical.rs no longer contains `pub fn canonicalize_type_name(`");
    // Read until the next top-level `fn ` (heuristic upper bound).
    let after = &CANONICAL_SRC[body_start..];
    // 2 KiB window is plenty — the fn is ~70 lines.
    let body_window = &after[..after.len().min(4096)];
    let combined = format!("{canonical_doc}\n{body_window}");

    let required_terms = ["Range", "Fun", "Unit", "alias"];
    let mut missing: Vec<&str> = Vec::new();
    for term in &required_terms {
        if !combined.contains(term) {
            missing.push(term);
        }
    }
    assert!(
        missing.is_empty(),
        "authoritative `canonicalize_type_name` in src/types/canonical.rs \
         no longer mentions the following collapse term(s): {missing:?}. \
         Either the rule was removed (in which case update the \
         typechecker mirror AND this test) or the wording changed in a \
         way this lock doesn't recognise. Required terms: {required_terms:?}."
    );
}
