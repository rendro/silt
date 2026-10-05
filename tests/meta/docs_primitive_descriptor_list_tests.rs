//! Regression lock for GAP(round 59): the globals page
//! (`docs/stdlib/globals.md`, which an editor shows for `println`) and
//! `docs/language/bindings-and-functions.md` both enumerate the
//! primitive type descriptors available in the global namespace
//! (`Int`, `Float`, `String`, `Bool`, …). A descriptor once went
//! undocumented in both; this test asserts every name of
//! `silt::module::BUILTIN_PRIMITIVE_NAMES` appears in both.

use std::fs;

/// The primitive descriptor names: `silt::module::BUILTIN_PRIMITIVE_NAMES`.
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

/// Every primitive descriptor is listed in the globals page, which an
/// editor shows on hover of `println`.
#[test]
fn globals_md_lists_every_primitive_descriptor() {
    let names = primitive_descriptor_names();
    let docs = silt::builtins::registry::docs::builtin_docs();
    let doc = docs.get("println").cloned().expect(
        "the globals page is the doc of `println` (and of the rest of the \
         prelude's names)",
    );
    for name in &names {
        let token = format!("`{name}`");
        assert!(
            doc.contains(&token),
            "docs/stdlib/globals.md is missing the primitive type descriptor \
             `{name}`. Add a row for it to the type-descriptor table."
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
             type descriptor `{name}`. Extend the prose sentence that \
             lists `Int`, `Float`, …"
        );
    }
}
