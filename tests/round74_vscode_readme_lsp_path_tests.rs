//! Round-74 GAP fix #6: `editors/vscode/README.md` references the
//! correct LSP module path.
//!
//! Before round 74, the README pointed at `src/lsp.rs`, which has not
//! existed since the LSP was split into a directory module rooted at
//! `src/lsp/mod.rs`. This is a pure path-correction lock.

const VSCODE_README_SRC: &str = include_str!("../editors/vscode/README.md");

#[test]
fn vscode_readme_does_not_reference_obsolete_lsp_path() {
    assert!(
        !VSCODE_README_SRC.contains("src/lsp.rs"),
        "editors/vscode/README.md must not reference `src/lsp.rs` — \
         the LSP lives at `src/lsp/mod.rs`."
    );
}

#[test]
fn vscode_readme_references_current_lsp_path() {
    assert!(
        VSCODE_README_SRC.contains("src/lsp/mod.rs") || VSCODE_README_SRC.contains("src/lsp/"),
        "editors/vscode/README.md should reference `src/lsp/mod.rs` (or \
         `src/lsp/` directory) so readers can find the capabilities advertisement."
    );
}
