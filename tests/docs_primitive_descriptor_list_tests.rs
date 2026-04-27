//! Regression lock for GAP(round 59): the `Globals` stdlib doc and
//! `docs/language/bindings-and-functions.md` both enumerate the set of
//! primitive type descriptors available in the global namespace
//! (`Int`, `Float`, `String`, `Bool`, …). Round 58 added `ExtFloat` to
//! the typechecker registration in `src/typechecker/builtins.rs`, but
//! both docs were not updated, leaving `ExtFloat` undocumented as a
//! top-level descriptor even though it works identically to the others.
//!
//! Round 62 phase-2 inlined the former `docs/stdlib/globals.md` into
//! `src/typechecker/builtins/docs.rs::GLOBALS_MD`. We now look up
//! `println`'s registered builtin doc as the surface presented to LSP
//! hover; the same prose carries the descriptor table.
//!
//! This test walks `src/typechecker/builtins.rs` to extract the
//! authoritative list of primitive descriptor names (the string
//! literals inside the `&["Int", "Float", "ExtFloat", "String",
//! "Bool"]` slice used by the registration loop) and asserts every one
//! of those names appears in both docs. If someone adds a new
//! primitive descriptor in the future, this test fires until the docs
//! list it too.

use std::fs;

/// Pull the source of truth out of `src/typechecker/builtins.rs`. We
/// look for the specific slice literal used to register primitive
/// descriptors so the test doesn't pick up unrelated string arrays. If
/// the code is refactored to a different shape, this test's fragile-
/// scraper error is clearer than a silent pass.
fn primitive_descriptor_names() -> Vec<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/typechecker/builtins.rs");
    let src =
        fs::read_to_string(path).expect("src/typechecker/builtins.rs must exist and be readable");

    // Find the slice literal: `&["Int", "Float", "ExtFloat", "String",
    // "Bool"]` (single-line). Round 58 wrote it exactly this way; if it
    // moves to multi-line or different form we'll need to update this
    // scraper.
    let marker_start = "for name in &[";
    let idx = src.find(marker_start).expect(
        "expected a `for name in &[...]` loop registering primitive descriptors in \
                 src/typechecker/builtins.rs. The scraper for this test needs updating.",
    );
    let after = &src[idx + marker_start.len()..];
    let end = after.find(']').expect(
        "expected closing `]` after primitive-descriptor slice literal in \
                 src/typechecker/builtins.rs",
    );
    let slice_body = &after[..end];

    let mut names = Vec::new();
    for token in slice_body.split(',') {
        let trimmed = token.trim().trim_matches('"').trim();
        if !trimmed.is_empty() {
            names.push(trimmed.to_string());
        }
    }

    assert!(
        !names.is_empty(),
        "expected at least one primitive descriptor name in \
         src/typechecker/builtins.rs, got none"
    );
    names
}

fn read_doc(rel: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Every primitive descriptor registered in the typechecker must be
/// listed in the Globals stdlib doc (round 62 phase-2: now embedded
/// in `super::docs::GLOBALS_MD` and surfaced via LSP hover on
/// `println`).
#[test]
fn globals_md_lists_every_primitive_descriptor() {
    let names = primitive_descriptor_names();
    let docs = silt::typechecker::builtin_docs();
    let doc = docs.get("println").cloned().expect(
        "globals.md prose is attached to `println` (and the rest of \
                 the unqualified globals); round 62 phase-2 inlined it via \
                 `attach_module_docs(env, GLOBALS_MD)` in \
                 src/typechecker/builtins.rs",
    );
    for name in &names {
        let token = format!("`{name}`");
        assert!(
            doc.contains(&token),
            "the Globals doc (now inlined as `super::docs::GLOBALS_MD` in \
             src/typechecker/builtins/docs.rs) is missing the primitive \
             type descriptor `{name}` (registered in \
             src/typechecker/builtins.rs). Add a row for it to the \
             primitive-type-descriptor table inside the GLOBALS_MD raw \
             string."
        );
    }
}

/// Same check against the language guide's bindings-and-functions
/// page, which enumerates the same descriptor names inline.
#[test]
fn bindings_and_functions_md_lists_every_primitive_descriptor() {
    let names = primitive_descriptor_names();
    let doc = read_doc("docs/language/bindings-and-functions.md");
    for name in &names {
        let token = format!("`{name}`");
        assert!(
            doc.contains(&token),
            "docs/language/bindings-and-functions.md is missing the primitive \
             type descriptor `{name}` (registered in \
             src/typechecker/builtins.rs). Extend the prose sentence that \
             lists `Int`, `Float`, …"
        );
    }
}

/// Sanity check: this test file's own scraper actually picks up
/// `ExtFloat` (the round-58 addition). If this fires, the scraper is
/// looking at the wrong slice literal.
#[test]
fn scraper_finds_extfloat_in_builtins_rs() {
    let names = primitive_descriptor_names();
    assert!(
        names.iter().any(|n| n == "ExtFloat"),
        "scraper did not find `ExtFloat` among {names:?}; if round 58's \
         addition is still in src/typechecker/builtins.rs, update the \
         scraper in this test. Otherwise the GAP regressed."
    );
}
