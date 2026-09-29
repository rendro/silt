//! End-to-end tests for two LSP features:
//!   - Workspace preload on initialize (indexes `.silt` files the
//!     editor has not yet opened).
//!   - Pull-model diagnostics (`textDocument/diagnostic`).
//!
//! Uses the shared LSP client in `support.rs`, which spawns `silt lsp`
//! as a subprocess and speaks LSP JSON-RPC over stdio.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde_json::json;

use crate::support::LspClient;

fn unique_tmp_dir(tag: &str) -> PathBuf {
    let n = REQ_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_preload_pull_{tag}_{n}"));
    // Clean any leftover from a prior aborted run.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir tempdir");
    dir
}

fn path_to_uri(p: &std::path::Path) -> String {
    let s = p.to_str().expect("utf8 path");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{}", s.replace('\\', "/"))
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn preload_indexes_unopened_files() {
    let dir = unique_tmp_dir("workspace");
    let file_a = dir.join("a.silt");
    let file_b = dir.join("b.silt");
    fs::write(&file_a, "fn alpha_fn() { 0 }\n").unwrap();
    // file_b defines a uniquely-named symbol; we never open it via
    // didOpen — the preloader must index it from disk at initialize.
    fs::write(&file_b, "fn preloaded_unique_sym() { 0 }\n").unwrap();

    let root_uri = path_to_uri(&dir);
    let mut client = LspClient::spawn_with_root(Some(&root_uri));

    // Without opening file_b, ask the server for workspace symbols
    // matching a substring that only file_b defines.
    let resp = client.request(
        "workspace/symbol",
        json!({ "query": "preloaded_unique_sym" }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let names: Vec<String> = arr
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    assert!(
        names.iter().any(|n| n == "preloaded_unique_sym"),
        "expected preloaded symbol from unopened file_b; got {names:?}"
    );

    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pull_diagnostic_returns_cached_errors() {
    let mut client = LspClient::spawn_with_root(None);
    let uri = "file:///tmp/silt_pull_diag.silt";
    // Known type error: undefined identifier.
    client.did_open_and_wait(uri, "fn main() { undefined_name }\n");

    let resp = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    let result = resp.get("result").expect("pull diagnostic result");
    // Full report shape: { kind: "full", items: [ ... ] }
    let items = result
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !items.is_empty(),
        "expected at least one diagnostic item from pull request; got {result}"
    );

    client.shutdown();
}
