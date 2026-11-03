//! End-to-end tests for two LSP features:
//!   - Workspace preload on initialize (indexes `.silt` files the
//!     editor has not yet opened).
//!   - Pull-model diagnostics (`textDocument/diagnostic`).
//!
//! Uses the shared LSP client in `support.rs`, which spawns `silt lsp`
//! as a subprocess and speaks LSP JSON-RPC over stdio.

use std::fs;
use std::path::PathBuf;

use serde_json::json;

use crate::support::LspClient;

fn unique_tmp_dir(tag: &str) -> PathBuf {
    let n = crate::support::next_id();
    let dir = std::env::temp_dir().join(format!(
        "silt_preload_pull_{tag}_{}_{n}",
        std::process::id()
    ));
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

/// A folder reached through a link is indexed, as the CLI walks it;
/// a link back into the workspace (a loop) and a second link to the
/// folder do not index its file twice.
#[cfg(unix)]
#[test]
fn preload_follows_a_linked_folder_once() {
    let dir = unique_tmp_dir("linked");
    let (ws, outside) = (dir.join("ws"), dir.join("outside"));
    fs::create_dir_all(&ws).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(ws.join("main.silt"), "fn main() { 0 }\n").unwrap();
    fs::write(outside.join("lib.silt"), "fn linked_unique_sym() { 0 }\n").unwrap();
    std::os::unix::fs::symlink(&outside, ws.join("linked")).unwrap();
    std::os::unix::fs::symlink(&outside, ws.join("linked_again")).unwrap();
    std::os::unix::fs::symlink(&ws, ws.join("back")).unwrap();

    let mut client = LspClient::spawn_with_root(Some(&path_to_uri(&ws)));
    let resp = client.request("workspace/symbol", json!({ "query": "linked_unique_sym" }));
    let found = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let names: Vec<&str> = found
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
        .collect();
    assert_eq!(names, ["linked_unique_sym"], "{resp}");

    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

/// The editor's requests are answered while the walk of the workspace
/// goes on, and the walk goes on to its end. A test hook of debug
/// builds holds the walk before each file while a gate file is there,
/// as a walk into a very large folder takes long: a request is answered
/// all the same; once the gate is gone, every file is indexed.
#[cfg(debug_assertions)]
#[test]
fn requests_are_answered_while_the_preload_walks() {
    use std::time::{Duration, Instant};

    let dir = unique_tmp_dir("walking");
    let ws = dir.join("ws");
    fs::create_dir_all(&ws).unwrap();
    fs::write(ws.join("a.silt"), "fn early_unique_sym() { 0 }\n").unwrap();
    fs::write(ws.join("z.silt"), "fn late_unique_sym() { 0 }\n").unwrap();
    let gate = dir.join("gate");
    fs::write(&gate, "").unwrap();

    let names = |resp: &serde_json::Value| -> Vec<String> {
        (resp.get("result").and_then(|r| r.as_array()).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect()
    };
    let gate_text = gate.to_str().expect("utf8 path");
    let mut client = LspClient::spawn_with_root_and_env(
        Some(&path_to_uri(&ws)),
        &[("SILT_LSP_PRELOAD_GATE", gate_text)],
    );
    let query = |name: &str| json!({ "query": name });
    let held = client.request_within(
        Duration::from_secs(20),
        "workspace/symbol",
        query("early_unique_sym"),
    );
    assert!(names(&held).is_empty(), "the walk is held: {held}");

    // The walk goes on once the gate is gone.
    fs::remove_file(&gate).unwrap();
    let start = Instant::now();
    loop {
        let late = client.request("workspace/symbol", query("late_unique_sym"));
        if names(&late) == ["late_unique_sym"] {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "never indexed: {late}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let early = client.request("workspace/symbol", query("early_unique_sym"));
    assert_eq!(names(&early), ["early_unique_sym"], "{early}");

    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

/// A pipe, a link to a device and a broken link named `.silt` are not
/// read: the walk goes past them to the files behind them (a pipe held
/// the preload for ever, a link to /dev/zero read without end). An
/// unreadable folder is in the log.
#[cfg(unix)]
#[test]
fn the_preload_reads_files_only() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = unique_tmp_dir("kinds");
    fs::write(dir.join("a.silt"), "fn first_kind_sym() { 0 }\n").unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(dir.join("m_pipe.silt"))
        .status();
    assert!(made.is_ok_and(|status| status.success()), "mkfifo");
    symlink("/dev/zero", dir.join("n_zero.silt")).unwrap();
    symlink(dir.join("gone"), dir.join("o_broken.silt")).unwrap();
    fs::create_dir_all(dir.join("p_shut")).unwrap();
    fs::set_permissions(dir.join("p_shut"), fs::Permissions::from_mode(0o000)).unwrap();
    fs::write(dir.join("z.silt"), "fn last_kind_sym() { 0 }\n").unwrap();

    let mut client = LspClient::spawn_with_root(Some(&path_to_uri(&dir)));
    let resp = client.request_within(
        std::time::Duration::from_secs(20),
        "workspace/symbol",
        json!({ "query": "kind_sym" }),
    );
    let mut names: Vec<String> = (resp.get("result").and_then(|r| r.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    names.sort();
    assert_eq!(names, ["first_kind_sym", "last_kind_sym"], "{resp}");
    // The line was written before the last file was indexed; it reaches
    // the client's copy of stderr a little later. (Root reads every
    // folder: there is no line then.)
    let readable = fs::read_dir(dir.join("p_shut")).is_ok();
    let start = std::time::Instant::now();
    let logged = |stderr: &str| stderr.contains("silt-lsp: preload: cannot read dir");
    while !readable
        && !logged(&client.stderr())
        && start.elapsed() < std::time::Duration::from_secs(20)
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let stderr = client.stderr();
    client.shutdown();
    let _ = fs::set_permissions(dir.join("p_shut"), fs::Permissions::from_mode(0o755));
    let _ = fs::remove_dir_all(&dir);
    assert!(readable || logged(&stderr), "{stderr}");
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
