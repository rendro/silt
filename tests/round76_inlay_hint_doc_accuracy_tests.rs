//! Round-76 D2-doc lock: the editor docs must accurately describe
//! what `src/lsp/inlay_hints.rs` actually emits.
//!
//! Background: prior wording in both `docs/editor-setup.md` and
//! `editors/vscode/README.md` claimed inlay hints render "inline
//! parameter names and inferred types". The implementation at
//! `src/lsp/inlay_hints.rs:7-19` only emits TYPE hints — namely,
//! inferred types for `let`-bindings and unannotated function
//! parameters. There is no parameter-name hint code path.
//!
//! Implementing parameter-name inlay hints is a deferred new feature.
//! The doc fix is to update the wording to match the current
//! behaviour. This test is a source-grep lock against future drift in
//! either direction.

use std::path::Path;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read_doc(rel: &str) -> String {
    let path = manifest_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

const EDITOR_SETUP_PATH: &str = "docs/editor-setup.md";
const EDITOR_SETUP_SRC: &str = include_str!("../docs/editor-setup.md");

const VSCODE_README_PATH: &str = "editors/vscode/README.md";
const VSCODE_README_SRC: &str = include_str!("../editors/vscode/README.md");

#[test]
fn editor_setup_doc_inlay_hint_wording_matches_impl() {
    // Sanity: include_str! and runtime read agree.
    let runtime = read_doc(EDITOR_SETUP_PATH);
    assert_eq!(
        EDITOR_SETUP_SRC, runtime,
        "include_str! and runtime read disagree for {EDITOR_SETUP_PATH}"
    );

    // Negative: the old wording promised "parameter names" hints —
    // the impl never emitted those. Drop the misleading phrase.
    assert!(
        !EDITOR_SETUP_SRC.contains("Inline parameter names and inferred types"),
        "docs/editor-setup.md must not claim inlay hints render `Inline parameter names \
         and inferred types` — `src/lsp/inlay_hints.rs` only emits type hints."
    );
    assert!(
        !EDITOR_SETUP_SRC.contains("parameter names"),
        "docs/editor-setup.md must not promise `parameter names` inlay hints — \
         the impl at `src/lsp/inlay_hints.rs` only emits type hints. If \
         parameter-name hints get implemented, update the impl AND this test."
    );

    // Positive: the new wording mentions both the let-binding case
    // and the unannotated-parameter case, matching the impl's two
    // code paths at `src/lsp/inlay_hints.rs:7-19`.
    assert!(
        EDITOR_SETUP_SRC.contains("Inferred types for `let`-bindings and unannotated parameters"),
        "docs/editor-setup.md should describe inlay hints as `Inferred types for \
         `let`-bindings and unannotated parameters rendered as inline hints` — \
         matching `src/lsp/inlay_hints.rs:7-19`."
    );
}

#[test]
fn vscode_readme_inlay_hint_wording_matches_impl() {
    let runtime = read_doc(VSCODE_README_PATH);
    assert_eq!(
        VSCODE_README_SRC, runtime,
        "include_str! and runtime read disagree for {VSCODE_README_PATH}"
    );

    // Negative: same misleading "parameter names" phrase.
    assert!(
        !VSCODE_README_SRC.contains("inline parameter names and inferred types"),
        "editors/vscode/README.md must not claim inlay hints render \
         `inline parameter names and inferred types` — the impl at \
         `src/lsp/inlay_hints.rs` only emits type hints."
    );
    assert!(
        !VSCODE_README_SRC.contains("parameter names"),
        "editors/vscode/README.md must not promise `parameter names` inlay hints — \
         the impl at `src/lsp/inlay_hints.rs` only emits type hints."
    );

    // Positive: the new wording.
    assert!(
        VSCODE_README_SRC.contains("inferred types for `let`-bindings and unannotated parameters"),
        "editors/vscode/README.md should describe inlay hints as `inferred types for \
         `let`-bindings and unannotated parameters rendered as inline hints` — \
         matching `src/lsp/inlay_hints.rs:7-19`."
    );
}
