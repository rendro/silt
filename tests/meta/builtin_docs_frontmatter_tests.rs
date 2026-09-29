//! Regression lock (GAP): raw YAML frontmatter (`---\ntitle: "…"\n…---`)
//! must never leak into the builtin-doc markdown the LSP renders on
//! hover / completion / signature-help.
//!
//! Every `*_MD` constant in `src/typechecker/builtins/docs.rs` begins
//! with the verbatim frontmatter its former `docs/stdlib/*.md` source
//! carried (website metadata: `title:` / `section:` / `order:`).
//! Per-`##`-section slicing (`iter_sections`) never sees it, but the
//! two WHOLE-document attach paths used to leak it:
//!
//!   * the `GLOBALS_MD` globals loop in
//!     `src/typechecker/builtins.rs::register_builtins` (every free
//!     function — `println`, `print`, `panic`, … — plus the documented
//!     prelude/import-gated constructors), and
//!   * `attach_module_overview` in `src/typechecker/builtins/docs.rs`
//!     (overview-only modules: bytes / crypto / uuid / stream / http /
//!     tcp / postgres / encoding / …),
//!
//! so hover on `println` rendered a stray horizontal rule followed by
//! raw `title: "Globals"` text. Both sites now route through
//! `strip_frontmatter`. Attaching the FULL page to globals is a
//! deliberate round-62 design decision and stays — only the
//! frontmatter is stripped.
//!
//! This walks EVERY registered builtin doc, so any future whole-doc
//! attach that forgets to strip trips it too.

#[test]
fn no_builtin_doc_leaks_yaml_frontmatter() {
    let docs = silt::typechecker::iter_builtin_docs();
    assert!(
        !docs.is_empty(),
        "iter_builtin_docs() returned nothing — builtin doc registration is broken"
    );
    for (name, body) in &docs {
        assert!(
            !body.starts_with("---\n"),
            "builtin doc for `{name}` starts with a raw YAML frontmatter \
             delimiter (`---`). Whole-document attach sites must route \
             through `strip_frontmatter` in \
             src/typechecker/builtins/docs.rs — see the GLOBALS_MD loop \
             in src/typechecker/builtins.rs::register_builtins and \
             `attach_module_overview`."
        );
        assert!(
            !body.contains("\ntitle: \""),
            "builtin doc for `{name}` contains a raw frontmatter \
             `title: \"…\"` line — YAML frontmatter leaked into hover \
             markdown. Route the attach site through `strip_frontmatter` \
             in src/typechecker/builtins/docs.rs."
        );
    }
}

/// The repro from the finding: hover on `println` surfaces the whole
/// Globals page (round-62 design decision), but it must begin with the
/// page prose, not `---\ntitle: "Globals"…` metadata.
#[test]
fn println_doc_starts_with_prose_not_frontmatter() {
    let docs = silt::typechecker::builtin_docs();
    let doc = docs
        .get("println")
        .expect("`println` must have a registered builtin doc (GLOBALS_MD)");
    assert!(
        !doc.starts_with("---"),
        "println's hover doc still starts with frontmatter:\n{}",
        doc.chars().take(120).collect::<String>()
    );
    assert!(
        doc.contains("`println`"),
        "println's hover doc lost the Globals prose entirely — \
         strip_frontmatter over-stripped? got:\n{}",
        doc.chars().take(120).collect::<String>()
    );
}

/// Same for a module-overview attach (`attach_module_overview`):
/// overview-only modules get the whole page, minus frontmatter.
#[test]
fn module_overview_doc_starts_with_prose_not_frontmatter() {
    let docs = silt::typechecker::builtin_docs();
    let doc = docs
        .get("crypto.sha256")
        .expect("`crypto.sha256` must have a registered builtin doc (CRYPTO_MD overview)");
    assert!(
        !doc.starts_with("---"),
        "crypto.sha256's hover doc still starts with frontmatter:\n{}",
        doc.chars().take(120).collect::<String>()
    );
    assert!(
        doc.contains("# crypto"),
        "crypto.sha256's overview doc lost the module heading — \
         strip_frontmatter over-stripped? got:\n{}",
        doc.chars().take(120).collect::<String>()
    );
}
