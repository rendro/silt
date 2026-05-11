//! Lock tests for round-85 LSP cleanup cluster.
//!
//! These guard three small dedup/shim fixes:
//!
//!   1. `expr_extent` returns plain `usize` (no dead `(usize, ())` slot).
//!   2. `selection_range::collect_decl_ranges` extracts a
//!      `try_push_body_extent` helper shared by `Decl::Fn`, `Decl::Let`,
//!      and each `Decl::TraitImpl` method arm.
//!   3. `inlay_hints` does not define a private `position_to_offset`
//!      shim — it imports the conversion directly.
//!
//! All locks are source-grep based; the implementations are exercised by
//! the existing LSP tier-2 / selection_range tests.

use std::fs;

const TEXT_UTILS_PATH: &str = "src/lsp/text_utils.rs";
const SELECTION_RANGE_PATH: &str = "src/lsp/selection_range.rs";
const INLAY_HINTS_PATH: &str = "src/lsp/inlay_hints.rs";

fn read(path: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

#[test]
fn expr_extent_signature_returns_usize() {
    let src = read(TEXT_UTILS_PATH);
    assert!(
        src.contains("fn expr_extent(expr: &Expr, source: &str) -> usize"),
        "expr_extent should return plain `usize`, not `(usize, ())`. \
         Source of {TEXT_UTILS_PATH} did not contain the expected signature."
    );
    assert!(
        !src.contains("-> (usize, ())"),
        "the dead `(usize, ())` return shape must be gone from {TEXT_UTILS_PATH}"
    );
}

#[test]
fn selection_range_decl_arms_use_helper() {
    let src = read(SELECTION_RANGE_PATH);
    assert!(
        src.contains("fn try_push_body_extent"),
        "selection_range.rs should define `try_push_body_extent` to dedup \
         the three decl arms"
    );
    let call_count = src.matches("try_push_body_extent(").count();
    // Three call sites in the decl arms; the `fn` definition itself does
    // not have `try_push_body_extent(` (it has `try_push_body_extent\n` or
    // `try_push_body_extent\n(` depending on formatting). To be robust we
    // require AT LEAST 3 `try_push_body_extent(` invocations.
    assert!(
        call_count >= 3,
        "selection_range.rs should call `try_push_body_extent(` from at \
         least 3 decl arms (Decl::Fn, Decl::Let, Decl::TraitImpl method); \
         found {call_count}"
    );
}

#[test]
fn inlay_hints_no_position_to_offset_shim() {
    let src = read(INLAY_HINTS_PATH);
    assert!(
        !src.contains("fn position_to_offset"),
        "inlay_hints.rs must not define a `position_to_offset` shim — \
         import the conversion from `super::conversions` instead"
    );
    // Sanity check: the file still uses position_to_offset, just by
    // import (or qualified path).
    assert!(
        src.contains("position_to_offset"),
        "inlay_hints.rs still needs to call position_to_offset somewhere"
    );
}
