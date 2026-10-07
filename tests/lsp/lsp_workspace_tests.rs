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
    client.did_open_and_wait(file_a, "pub fn shared_helper(x) { x + 1 }\n");
    client.did_open_and_wait(
        file_b,
        "import silt_wspace_a.{ shared_helper }\nfn main() { shared_helper(5) }\n",
    );

    let resp = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": file_b },
            "position": { "line": 1, "character": 15 }
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
    client.did_open_and_wait(file_a, "pub fn pinger(x) { x }\nfn main() { pinger(1) }\n");
    client.did_open_and_wait(
        file_b,
        "import silt_wspace_ref_a\nfn other() { silt_wspace_ref_a.pinger(2) }\n",
    );

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

/// Two open documents in one directory import each other whether or not
/// the directory is on disk: `file:///tmp/...` names no directory on
/// Windows, and an editor's unsaved files may name none anywhere.
#[test]
fn open_documents_of_a_directory_that_is_not_on_disk_import_each_other() {
    let mut client = LspClient::spawn();
    let dir = format!("file:///silt_no_such_dir_{}", std::process::id());
    let file_a = format!("{dir}/wspace_missing_a.silt");
    let file_b = format!("{dir}/wspace_missing_b.silt");
    client.did_open_and_wait(&file_a, "pub fn pinger(x) { x }\nfn main() { pinger(1) }\n");
    client.did_open_and_wait(
        &file_b,
        "import wspace_missing_a\nfn other() { wspace_missing_a.pinger(2) }\n",
    );

    let resp = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": file_a },
            "position": { "line": 1, "character": 15 },
            "context": { "includeDeclaration": true }
        }),
    );
    let uris: Vec<String> = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("references result is an array")
        .iter()
        .filter_map(|loc| loc.get("uri").and_then(|u| u.as_str()).map(String::from))
        .collect();
    assert!(
        uris.iter().any(|u| *u == file_b),
        "expected a reference in the importing document; got: {uris:?}"
    );
    client.shutdown();
}

/// An importer is checked again, and its diagnostics published again,
/// when the document it imports opens and when it closes: the error at
/// the `import` goes away with the first and comes back with the second.
#[test]
fn an_importer_is_checked_again_when_the_imported_document_opens_or_closes() {
    let mut client = LspClient::spawn();
    let dir = format!("file:///silt_no_such_dir_{}_stale", std::process::id());
    let imported = format!("{dir}/wspace_late_a.silt");
    let importer = format!("{dir}/wspace_late_b.silt");
    let messages = |published: &Value| -> Vec<String> {
        published
            .pointer("/params/diagnostics")
            .and_then(|d| d.as_array())
            .into_iter()
            .flatten()
            .filter_map(|d| d.get("message").and_then(|m| m.as_str()).map(String::from))
            .collect()
    };

    let first = client.did_open_and_wait(
        &importer,
        "import wspace_late_a\nfn other() { wspace_late_a.pinger(2) }\n",
    );
    assert!(
        messages(&first)
            .iter()
            .any(|m| m.contains("cannot load module")),
        "the import of a file that is nowhere is an error; got {first}"
    );

    client.did_open(&imported, "pub fn pinger(x) { x }\n");
    let reopened = client.wait_for_diagnostics(&importer);
    assert_eq!(
        messages(&reopened),
        Vec::<String>::new(),
        "the importer has no error once the imported document is open"
    );

    client.send_notification(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": imported } }),
    );
    let closed = client.wait_for_diagnostics(&importer);
    assert!(
        messages(&closed)
            .iter()
            .any(|m| m.contains("cannot load module")),
        "the error is back once the imported document is closed; got {closed}"
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
        "pub fn renamed_target() { 0 }\nfn main() { renamed_target() }\n",
    );
    client.did_open_and_wait(
        file_b,
        "import silt_wspace_rn_a.{ renamed_target }\nfn caller() { renamed_target() }\n",
    );

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
