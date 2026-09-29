//! End-to-end LSP tests for Tier 1 workspace features:
//! cross-file goto-def, references, rename, workspace/symbol.
//!
//! Uses the shared LSP client in `support.rs`, which spawns `silt lsp`
//! as a subprocess and speaks LSP JSON-RPC over stdio.

use serde_json::{Value, json};

use crate::support::LspClient;

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn cross_file_definition() {
    let mut client = LspClient::spawn();
    let file_a = "file:///tmp/silt_wspace_a.silt";
    let file_b = "file:///tmp/silt_wspace_b.silt";
    client.did_open_and_wait(file_a, "fn shared_helper(x) { x + 1 }\n");
    client.did_open_and_wait(file_b, "fn main() { shared_helper(5) }\n");

    let resp = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": file_b },
            "position": { "line": 0, "character": 15 }
        }),
    );
    let result = resp.get("result").expect("definition result");
    let uri = match result {
        Value::Object(obj) => obj.get("uri").and_then(|v| v.as_str()).map(String::from),
        Value::Array(arr) if !arr.is_empty() => {
            arr[0].get("uri").and_then(|v| v.as_str()).map(String::from)
        }
        _ => None,
    };
    assert_eq!(
        uri.as_deref(),
        Some(file_a),
        "expected cross-file goto to land in file_a; got: {result}"
    );
    client.shutdown();
}

#[test]
fn references_finds_all_uses_across_files() {
    let mut client = LspClient::spawn();
    let file_a = "file:///tmp/silt_wspace_ref_a.silt";
    let file_b = "file:///tmp/silt_wspace_ref_b.silt";
    client.did_open_and_wait(file_a, "fn pinger(x) { x }\nfn main() { pinger(1) }\n");
    client.did_open_and_wait(file_b, "fn other() { pinger(2) }\n");

    // Click on the `pinger` call at line 1 (inside main's body).
    let resp = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": file_a },
            "position": { "line": 1, "character": 15 },
            "context": { "includeDeclaration": true }
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("references result is an array");
    let uris: Vec<String> = arr
        .iter()
        .filter_map(|loc| loc.get("uri").and_then(|u| u.as_str()).map(String::from))
        .collect();
    assert!(
        uris.iter().any(|u| u == file_a),
        "expected a reference in file_a; got: {uris:?}"
    );
    assert!(
        uris.iter().any(|u| u == file_b),
        "expected a reference in file_b; got: {uris:?}"
    );
    client.shutdown();
}

#[test]
fn rename_returns_workspace_edit() {
    let mut client = LspClient::spawn();
    let file_a = "file:///tmp/silt_wspace_rn_a.silt";
    let file_b = "file:///tmp/silt_wspace_rn_b.silt";
    client.did_open_and_wait(
        file_a,
        "fn renamed_target() { 0 }\nfn main() { renamed_target() }\n",
    );
    client.did_open_and_wait(file_b, "fn caller() { renamed_target() }\n");

    // Click on the `renamed_target` call at line 1 (inside main's body).
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": file_a },
            "position": { "line": 1, "character": 18 },
            "newName": "fresh_name"
        }),
    );
    let changes = resp
        .get("result")
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    assert!(
        changes.contains_key(file_a),
        "expected edits in file_a; got {changes:?}"
    );
    assert!(
        changes.contains_key(file_b),
        "expected edits in file_b; got {changes:?}"
    );
    client.shutdown();
}

#[test]
fn rename_rejects_invalid_identifier() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_wspace_rn_bad.silt";
    client.did_open_and_wait(file, "fn foo() { 0 }\n");

    // Need a call site we can click on for the rename cursor.
    // Simpler: use a program with both a definition and a reference.
    let _ = file;
    let file2 = "file:///tmp/silt_wspace_rn_bad2.silt";
    client.did_open_and_wait(file2, "fn foo() { 0 }\nfn main() { foo() }\n");
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": file2 },
            "position": { "line": 1, "character": 13 },
            "newName": "not a valid name"
        }),
    );
    assert!(
        resp.get("error").is_some(),
        "expected error for invalid rename target; got {resp}"
    );
    client.shutdown();
}

#[test]
fn workspace_symbol_returns_matches_across_files() {
    let mut client = LspClient::spawn();
    let file_a = "file:///tmp/silt_wspace_sym_a.silt";
    let file_b = "file:///tmp/silt_wspace_sym_b.silt";
    client.did_open_and_wait(file_a, "fn alpha_fn() { 0 }\nfn beta_fn() { 0 }\n");
    client.did_open_and_wait(file_b, "fn gamma_fn() { 0 }\ntype AlphaType { x: Int }\n");

    let resp = client.request("workspace/symbol", json!({ "query": "alpha" }));
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("workspace/symbol returns array for 'alpha'");
    let names: Vec<String> = arr
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    assert!(
        names.iter().any(|n| n == "alpha_fn"),
        "expected alpha_fn; got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "AlphaType"),
        "expected AlphaType; got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "beta_fn"),
        "beta_fn shouldn't match 'alpha'; got {names:?}"
    );
    client.shutdown();
}
