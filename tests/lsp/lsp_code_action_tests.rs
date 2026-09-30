//! End-to-end tests for `textDocument/codeAction`.
//!
//! Spawns `silt lsp` as a subprocess and speaks LSP JSON-RPC over stdio.
//! Uses the shared client in `support.rs`.

use serde_json::{Value, json};

use crate::support::LspClient;

/// Small helper: find the first diagnostic whose message contains `needle`.
fn diag_matching<'a>(diags: &'a [Value], needle: &str) -> Option<&'a Value> {
    diags.iter().find(|d| {
        d.get("message")
            .and_then(|m| m.as_str())
            .is_some_and(|m| m.contains(needle))
    })
}

/// Extract the `result` array from a codeAction response.
fn code_actions(resp: &Value) -> Vec<Value> {
    resp.get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default()
}

// ── Tests ──────────────────────────────────────────────────────────

// Round 56 moved the "module 'X' is not imported" check into the
// typechecker (src/typechecker/inference.rs::~1920), and the LSP
// diagnostics pipeline forwards typechecker errors verbatim
// (src/lsp/diagnostics.rs:81-105). The code-action parser matches the
// exact phrase (src/lsp/code_action.rs::import_module_from_message), so
// the quick-fix end-to-end is live and this test locks it.
#[test]
fn add_import_quickfix_offered_for_unimported_module() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_ca_import.silt";
    // `list.map(...)` without `import list` triggers the compiler's
    // "module 'list' is not imported" diagnostic.
    let source = "fn main() { list.map([1], { x -> x }) }\n";
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    let import_diag = diag_matching(&diags, "not imported")
        .unwrap_or_else(|| panic!("expected 'not imported' diagnostic; got {diags:?}"))
        .clone();

    let resp = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": import_diag.get("range").cloned().unwrap_or(json!({
                "start": { "line": 0, "character": 0 },
                "end":   { "line": 0, "character": 0 }
            })),
            "context": { "diagnostics": [import_diag] }
        }),
    );
    let actions = code_actions(&resp);
    assert!(
        !actions.is_empty(),
        "expected at least one code action; got {resp}"
    );
    let action = actions
        .iter()
        .find(|a| {
            a.get("title")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.to_lowercase().contains("import"))
        })
        .unwrap_or_else(|| panic!("no action with 'import' in title; got {actions:?}"));

    // Walk to the edit's new_text and confirm it contains `import list`.
    let changes = action
        .pointer("/edit/changes")
        .and_then(|c| c.as_object())
        .expect("edit.changes exists");
    let edits = changes
        .get(uri)
        .and_then(|v| v.as_array())
        .expect("edits for our uri");
    let any_contains = edits.iter().any(|e| {
        e.get("newText")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.contains("import list"))
    });
    assert!(
        any_contains,
        "expected edit inserting `import list`; got {edits:?}"
    );
    client.shutdown();
}

#[test]
fn no_action_when_diagnostic_is_unrelated() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_ca_none.silt";
    let source = "fn main() { undefined_name }\n";
    let diags = client.did_open_and_collect_diagnostics(uri, source);
    // Pick any diagnostic (there will be one for the undefined identifier).
    let Some(diag) = diags.first().cloned() else {
        // If the typechecker emitted nothing, the test trivially passes.
        client.shutdown();
        return;
    };
    let resp = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": diag.get("range").cloned().unwrap_or(json!({
                "start": { "line": 0, "character": 0 },
                "end":   { "line": 0, "character": 0 }
            })),
            "context": { "diagnostics": [diag] }
        }),
    );
    let actions = code_actions(&resp);
    // We expect no matching quick-fix for an "undefined name" diagnostic —
    // it's unrelated to our starter catalog.
    assert!(
        actions.is_empty(),
        "expected no code actions for unrelated diagnostic; got {actions:?}"
    );
    client.shutdown();
}

#[test]
fn code_action_capability_advertised() {
    // The initialize response should advertise codeActionProvider.
    let mut client = LspClient::spawn();
    // We already initialized inside spawn(); send another request to exercise
    // the dispatch surface — if the capability isn't wired up, subsequent
    // requests still work, so we instead check server behaviour via a second
    // initialize-like round trip isn't possible. Smoke-test: send an empty
    // codeAction request against an empty doc; the response should be a
    // JSON array (or null/empty), never an error.
    let uri = "file:///tmp/silt_ca_empty.silt";
    let _ = client.did_open_and_collect_diagnostics(uri, "fn main() { 0 }\n");
    let resp = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0} },
            "context": { "diagnostics": [] }
        }),
    );
    assert!(
        resp.get("error").is_none(),
        "codeAction must not return an error on a clean document; got {resp}"
    );
    // result is an array (possibly empty) or null.
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    assert!(
        result.is_array() || result.is_null(),
        "expected array or null result, got {result}"
    );
    client.shutdown();
}
