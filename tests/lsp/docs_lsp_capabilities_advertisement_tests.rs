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
//! Round 65+ extended the lock to cover the full set of advertised capabilities
//! (see `src/lsp/mod.rs:421-468`). Eight capabilities had been silently added
//! without ever being documented:
//!
//! - workspace_symbol_provider
//! - inlay_hint_provider
//! - document_highlight_provider
//! - folding_range_provider
//! - selection_range_provider
//! - type_definition_provider
//! - implementation_provider
//! - code_action_provider
//!
//! These tests lock the bidirectional invariant: if a future edit removes one
//! of the advertised capabilities OR the docs drop a feature, this test fires.
//! The advertised set is read from a live `initialize` response.

use serde_json::{Map, Value};

use crate::support::LspClient;

const README: &str = include_str!("../../README.md");
const EDITOR_SETUP: &str = include_str!("../../docs/editor-setup.md");
const VSCODE_PACKAGE: &str = include_str!("../../editors/vscode/package.json");

/// The `capabilities` object of a live `initialize` response.
fn advertised_capabilities() -> Map<String, Value> {
    let mut client = LspClient::spawn_uninitialized();
    let (_, resp) = client.initialize();
    client.shutdown();
    resp.pointer("/result/capabilities")
        .and_then(|c| c.as_object())
        .cloned()
        .unwrap_or_else(|| panic!("initialize response has no capabilities: {resp}"))
}

/// Natural-language phrases that must appear in user-facing docs for each
/// advertised capability. The phrase is matched case-insensitively as a
/// substring; the second tuple element is the capability's key in the
/// `initialize` response, for diagnostic messages and the bidirectional lock.
const DOC_PHRASES: &[(&str, &str)] = &[
    ("hover", "hoverProvider"),
    ("go to definition", "definitionProvider"),
    ("completion", "completionProvider"),
    ("signature help", "signatureHelpProvider"),
    ("document symbols", "documentSymbolProvider"),
    ("formatting", "documentFormattingProvider"),
    ("references", "referencesProvider"),
    ("rename", "renameProvider"),
    ("semantic tokens", "semanticTokensProvider"),
    ("diagnostics", "diagnosticProvider"),
    ("workspace symbol", "workspaceSymbolProvider"),
    ("inlay hint", "inlayHintProvider"),
    ("document highlight", "documentHighlightProvider"),
    ("folding range", "foldingRangeProvider"),
    ("selection range", "selectionRangeProvider"),
    ("type definition", "typeDefinitionProvider"),
    ("implementation", "implementationProvider"),
    ("code action", "codeActionProvider"),
];

/// Normalize for substring matching: lowercase + collapse hyphens to spaces.
/// Docs commonly write "go-to-definition" while a natural phrase reads
/// "go to definition"; both should satisfy the lock.
fn normalize(s: &str) -> String {
    s.to_lowercase().replace('-', " ")
}

#[test]
fn readme_lists_advertised_lsp_capabilities() {
    let lower = normalize(README);
    for (needle, cap) in DOC_PHRASES {
        assert!(
            lower.contains(needle),
            "README.md LSP feature list must mention `{needle}` — \
             the LSP server advertises `{cap}` as a capability \
             (see the `initialize` response built in src/lsp/mod.rs). If the capability has truly been \
             dropped, also remove it from src/lsp/mod.rs in the same change."
        );
    }
}

#[test]
fn editor_setup_lists_advertised_lsp_capabilities() {
    let lower = normalize(EDITOR_SETUP);
    for (needle, cap) in DOC_PHRASES {
        assert!(
            lower.contains(needle),
            "docs/editor-setup.md LSP features table must mention `{needle}` — \
             the LSP server advertises `{cap}` as a capability \
             (see the `initialize` response built in src/lsp/mod.rs). If the capability has truly been \
             dropped, also remove it from src/lsp/mod.rs in the same change."
        );
    }
}

#[test]
fn lsp_server_still_advertises_documented_capabilities() {
    // Bidirectional lock: if a future edit removes any of these capabilities
    // from the initialize response, the docs are now lying. Fire so the
    // doc-and-code go out of sync immediately and the author has to choose:
    // keep the capability or drop it from the docs in the same change.
    let caps = advertised_capabilities();
    for (_phrase, cap) in DOC_PHRASES {
        assert!(
            caps.contains_key(*cap),
            "the LSP server must still advertise `{cap}` in its initialize \
             response — README.md and docs/editor-setup.md tell users this \
             capability exists. If the capability has truly been dropped, also \
             remove the corresponding row from docs/editor-setup.md and the \
             feature list in README.md. Advertised: {:?}",
            caps.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn vscode_extension_description_lists_advertised_lsp_capabilities() {
    // The VS Code extension launches the same `silt lsp` server, so it gets
    // every advertised capability. Lock the description string so it cannot
    // silently drift back to listing only a handful of features. We only
    // require the phrase substring (case-insensitive); the description is
    // length-bounded and may use shortened forms.
    let lower = normalize(VSCODE_PACKAGE);
    for (needle, cap) in DOC_PHRASES {
        assert!(
            lower.contains(needle),
            "editors/vscode/package.json description must mention `{needle}` — \
             the VS Code extension launches `silt lsp` and gets `{cap}` \
             as an advertised capability (see the `initialize` response built in src/lsp/mod.rs)."
        );
    }
}

#[test]
fn every_advertised_provider_is_documented() {
    // Every `*Provider` capability the server announces in its initialize
    // response must be covered by `DOC_PHRASES`. This is the strict
    // bidirectional lock: adding a new provider without updating the docs
    // (and this table) fires immediately. `textDocumentSync` is not a
    // provider — it is a protocol-level capability, not a user-visible
    // feature.
    let documented: std::collections::HashSet<&str> =
        DOC_PHRASES.iter().map(|(_, cap)| *cap).collect();
    let caps = advertised_capabilities();
    let missing: Vec<&String> = caps
        .keys()
        .filter(|name| name.ends_with("Provider") && !documented.contains(name.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "the LSP server advertises provider capabilities not covered by \
         DOC_PHRASES in this test (and therefore likely not mentioned in \
         README.md or docs/editor-setup.md): {missing:?}. \
         Add a row to DOC_PHRASES and update README.md + docs/editor-setup.md \
         + editors/vscode/package.json to document the new capability."
    );
}
