//! Regression lock (GAP): raw YAML frontmatter (`---\ntitle: "…"\n…---`)
//! must never leak into the builtin-doc markdown the LSP renders on
//! hover / completion / signature-help.
//!
//! Every page of `docs/stdlib/` begins with frontmatter for the docs
//! website (`title:` / `section:` / `order:`). A name's own section
//! never includes it, but a name that shows a whole page would: the
//! prelude's names show the globals page, and a function with no
//! section of its own shows its module's page. Hover on `println` once
//! rendered a stray horizontal rule followed by raw `title: "Globals"`
//! text; `registry::docs::builtin_docs` strips it.
//!
//! This walks every builtin doc.

#[test]
fn no_builtin_doc_leaks_yaml_frontmatter() {
    let docs = silt::builtins::registry::docs::builtin_docs();
    assert!(!docs.is_empty(), "no builtin name has a doc");
    for (name, body) in docs {
        assert!(
            !body.starts_with("---\n"),
            "builtin doc for `{name}` starts with a raw YAML frontmatter \
             delimiter (`---`). A name that shows a whole page shows it \
             through `strip_frontmatter` \
             (src/builtins/registry/docs.rs::builtin_docs)."
        );
        assert!(
            !body.contains("\ntitle: \""),
            "builtin doc for `{name}` contains a raw frontmatter \
             `title: \"…\"` line — YAML frontmatter leaked into hover \
             markdown. A whole page is shown through `strip_frontmatter` \
             (src/builtins/registry/docs.rs)."
        );
    }
}

/// The repro from the finding: hover on `println` surfaces the whole
/// Globals page (round-62 design decision), but it must begin with the
/// page prose, not `---\ntitle: "Globals"…` metadata.
#[test]
fn println_doc_starts_with_prose_not_frontmatter() {
    let docs = silt::builtins::registry::docs::builtin_docs();
    let doc = docs
        .get("println")
        .expect("`println` has a builtin doc: the globals page");
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

/// Same for a function with no section of its own: it shows its
/// module's whole page, minus frontmatter.
#[test]
fn module_overview_doc_starts_with_prose_not_frontmatter() {
    let docs = silt::builtins::registry::docs::builtin_docs();
    let doc = docs
        .get("crypto.sha256")
        .expect("`crypto.sha256` has a builtin doc: the crypto page");
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
