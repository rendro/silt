//! Regression lock for GAP(round 59): the `Globals` stdlib doc and
//! `docs/language/bindings-and-functions.md` both enumerate the set of
//! primitive type descriptors available in the global namespace
//! (`Int`, `Float`, `String`, `Bool`, …). A descriptor added to the
//! typechecker registration in `src/typechecker/builtins.rs` once went
//! undocumented in both docs; this test keeps them in step.
//!
//! Round 62 phase-2 inlined the former `docs/stdlib/globals.md` into
//! `src/typechecker/builtins/docs.rs::GLOBALS_MD`. We now look up
//! `println`'s registered builtin doc as the surface presented to LSP
//! hover; the same prose carries the descriptor table.
//!
//! This test walks `src/typechecker/builtins.rs` to extract the
//! authoritative list of primitive descriptor names (the string
//! literals inside the `&["Int", "Float", "String", "Bool"]` slice used
//! by the registration loop) and asserts every one
//! of those names appears in both docs. If someone adds a new
//! primitive descriptor in the future, this test fires until the docs
//! list it too.

use std::fs;

/// Pull the source of truth out of `src/module.rs::BUILTIN_PRIMITIVE_NAMES`.
///
/// Round-73 BLOAT-2 hoisted the primitive-descriptor name set out of
/// `src/typechecker/builtins.rs` (where it had been a `for name in
/// &[...]` array literal) onto the constant
/// `module::BUILTIN_PRIMITIVE_NAMES` in `src/module.rs`. Both the VM's
/// dispatch loop and the typechecker's registration loop now iterate
/// the same constant. This scraper follows the source of truth to its
/// new home.
fn primitive_descriptor_names() -> Vec<String> {
    // Use the public constant directly — no source-text scraping
    // needed once the names are exposed as a `pub const` slice.
    silt::module::BUILTIN_PRIMITIVE_NAMES
        .iter()
        .map(|s| (*s).to_string())
        .collect()
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
