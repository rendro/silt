//! Source-grep lock tests for rustdoc warnings introduced by intra-doc
//! links to private items, redundant explicit link targets, or HTML-like
//! sequences inside doc comments.
//!
//! Each test asserts on the file's textual contents — running
//! `cargo doc --no-deps --release` from a test would be too slow, so we
//! pin the fixed strings here. If you legitimately need to reintroduce
//! one of the patterns below, update the corresponding test and verify
//! `cargo doc --no-deps --release 2>&1 | grep warning:` is still clean.
//!
//! This file is shared between agents — append new tests at the bottom
//! using unique test names; do not replace existing tests.

use std::fs;

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[test]
fn lockfile_doc_no_private_link() {
    // src/lockfile.rs `matches_manifest` doc must not intra-doc-link to
    // the private `Lockfile::resolve_offline` (rustdoc emits a warning
    // for public docs linking private items).
    let src = read("src/lockfile.rs");
    assert!(
        !src.contains("[`Lockfile::resolve_offline`]"),
        "src/lockfile.rs must not contain intra-doc link `[`Lockfile::resolve_offline`]` \
         in public docs (rustdoc warns: public doc links to private item)"
    );
}

#[test]
fn scheduler_doc_no_private_link() {
    // src/scheduler.rs `submit` doc must not intra-doc-link to the
    // private `MAX_TASKS` constant.
    let src = read("src/scheduler.rs");
    // The warning fires on the doc-comment form `[`MAX_TASKS`]`. Plain
    // code references like `MAX_TASKS` (no leading `[`) inside the
    // implementation are fine.
    assert!(
        !src.contains("[`MAX_TASKS`]"),
        "src/scheduler.rs must not contain intra-doc link `[`MAX_TASKS`]` \
         in public docs (rustdoc warns: public doc links to private item)"
    );
}

#[test]
fn suggest_doc_no_unescaped_html_tag() {
    // src/typechecker/suggest.rs module doc must not contain an
    // unescaped `<candidate>` sequence — rustdoc parses it as an
    // unclosed HTML tag.
    let src = read("src/typechecker/suggest.rs");
    assert!(
        !src.contains("<candidate>"),
        "src/typechecker/suggest.rs must not contain unescaped `<candidate>` \
         in doc comments (rustdoc warns: unclosed HTML tag `candidate`)"
    );
    // Also guard against the obvious sibling forms that would re-trigger
    // the same warning class.
    assert!(
        !src.contains("<typo>"),
        "src/typechecker/suggest.rs must not contain unescaped `<typo>` in doc comments"
    );
    assert!(
        !src.contains("<mod>"),
        "src/typechecker/suggest.rs must not contain unescaped `<mod>` in doc comments"
    );
}

// ── Round 76 L5canon: src/types/canonical.rs rustdoc fixes ──────────

/// `AliasInfo` doc used to intra-doc-link `[`TypeExpr`]` (a bare
/// item name). The `TypeExpr` type lives at `crate::ast::TypeExpr`,
/// not in `types::canonical`'s scope, so rustdoc emitted "unresolved
/// link" warnings. Round 76 L5canon switches the link to the fully-
/// qualified path `[crate::ast::TypeExpr]`.
#[test]
fn canonical_intra_doc_link_uses_full_path() {
    let src = read("src/types/canonical.rs");
    // The bare form must not be present any more.
    assert!(
        !src.contains("[`TypeExpr`]"),
        "src/types/canonical.rs must not contain bare intra-doc link `[`TypeExpr`]` \
         (rustdoc warns: unresolved link); use the fully-qualified `[crate::ast::TypeExpr]` form."
    );
}

/// `canonical_name` doc used to write `[`Symbol`](crate::intern::Symbol)`
/// — a redundant explicit link target. Rustdoc resolves the bare
/// `[`Symbol`]` form from the in-scope `use crate::intern::{Symbol,
/// ...}` import and emits a "redundant explicit link target"
/// warning when both the link text and the target are spelled out.
/// Round 76 L5canon strips the redundant target.
#[test]
fn canonical_no_redundant_link_target() {
    let src = read("src/types/canonical.rs");
    assert!(
        !src.contains("[`Symbol`](crate::intern::Symbol)"),
        "src/types/canonical.rs must not contain redundant explicit link target \
         `[`Symbol`](crate::intern::Symbol)` (rustdoc warns: redundant explicit link target); \
         the bare `[`Symbol`]` form resolves through the in-scope `use`."
    );
}
