//! Lock test: keep the documented LSP feature lists synchronized with what the
//! server actually advertises in `src/lsp/mod.rs`.
//!
//! Round 62 (commit 5b3f240) hardened references, rename, and semantic tokens
//! for record-shorthand binders, anonymous-record destructure, and trait
//! default-method bodies. The capabilities have been wired into the server
//! initialize response (`src/lsp/mod.rs:436-437,454-457`) but neither
//! `README.md` nor `docs/editor-setup.md` mentioned them, so users had no way
//! to discover that those features exist.
//!
//! These tests lock the bidirectional invariant: if a future edit removes one
//! of the advertised capabilities OR the docs drop a feature, this test fires.

const README: &str = include_str!("../README.md");
const EDITOR_SETUP: &str = include_str!("../docs/editor-setup.md");
const LSP_MOD: &str = include_str!("../src/lsp/mod.rs");

#[test]
fn readme_lists_advertised_lsp_capabilities() {
    let lower = README.to_lowercase();
    for needle in &["rename", "references", "semantic tokens"] {
        assert!(
            lower.contains(needle),
            "README.md LSP feature list must mention `{needle}` — \
             the LSP server advertises it as a capability \
             (see src/lsp/mod.rs:436-437,454-457)"
        );
    }
}

#[test]
fn editor_setup_lists_advertised_lsp_capabilities() {
    let lower = EDITOR_SETUP.to_lowercase();
    for needle in &["rename", "references", "semantic tokens"] {
        assert!(
            lower.contains(needle),
            "docs/editor-setup.md LSP features table must mention `{needle}` — \
             the LSP server advertises it as a capability \
             (see src/lsp/mod.rs:436-437,454-457)"
        );
    }
}

#[test]
fn lsp_server_still_advertises_documented_capabilities() {
    // Bidirectional lock: if a future edit removes any of these capabilities
    // from the initialize response, the docs are now lying. Fire so the
    // doc-and-code go out of sync immediately and the author has to choose:
    // keep the capability or drop it from the docs in the same change.
    for needle in &[
        "references_provider",
        "rename_provider",
        "semantic_tokens_provider",
    ] {
        assert!(
            LSP_MOD.contains(needle),
            "src/lsp/mod.rs must still advertise `{needle}` — \
             README.md and docs/editor-setup.md tell users this capability \
             exists. If the capability has truly been dropped, also remove \
             the corresponding row from docs/editor-setup.md and the feature \
             list in README.md."
        );
    }
}
