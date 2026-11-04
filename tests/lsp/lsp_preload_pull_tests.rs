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

/// A request about one document is answered while the walk of the
/// workspace goes on, and the walk goes on to its end. A test hook of
/// debug builds holds the walk before each file while a gate file is
/// there, as a walk into a very large folder takes long.
#[cfg(debug_assertions)]
#[test]
fn requests_are_answered_while_the_preload_walks() {
    use std::time::Duration;

    let dir = unique_tmp_dir("walking");
    let ws = dir.join("ws");
    fs::create_dir_all(&ws).unwrap();
    fs::write(ws.join("a.silt"), "fn early_unique_sym() { 0 }\n").unwrap();
    fs::write(ws.join("z.silt"), "fn late_unique_sym() { 0 }\n").unwrap();
    let gate = dir.join("gate");
    fs::write(&gate, "").unwrap();

    let gate_text = gate.to_str().expect("utf8 path");
    let mut client = LspClient::spawn_with_root_and_env(
        Some(&path_to_uri(&ws)),
        &[("SILT_LSP_PRELOAD_GATE", gate_text)],
    );
    let open = path_to_uri(&dir.join("open.silt"));
    client.did_open_and_wait(&open, "fn open_unique_sym() { 0 }\n");
    let held = client.request_within(
        Duration::from_secs(20),
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": open } }),
    );
    let names: Vec<&str> = (held["result"].as_array().into_iter().flatten())
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(names, ["open_unique_sym"], "{held}");
    assert!(gate.exists(), "the walk is held");

    // The walk goes on once the gate is gone.
    fs::remove_file(&gate).unwrap();
    let all = client.request_within(
        Duration::from_secs(20),
        "workspace/symbol",
        json!({ "query": "unique_sym" }),
    );
    let mut names: Vec<&str> = (all["result"].as_array().into_iter().flatten())
        .filter_map(|s| s["name"].as_str())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["early_unique_sym", "late_unique_sym", "open_unique_sym"],
        "{all}"
    );

    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

/// A rename asked while the walk of the workspace goes on waits for its
/// end: it covers every file that names what it renames, not only the
/// ones found so far.
#[cfg(debug_assertions)]
#[test]
fn a_rename_while_the_preload_walks_covers_every_file() {
    use std::time::Duration;

    const IMPORTERS: usize = 5;
    let dir = unique_tmp_dir("rename_walking");
    let ws = dir.join("ws");
    fs::create_dir_all(&ws).unwrap();
    let lib = "pub fn shared(x: Int) -> Int {\n  x\n}\n";
    fs::write(ws.join("lib.silt"), lib).unwrap();
    for i in 0..IMPORTERS {
        let text = format!("import lib\n\npub fn g{i}() -> Int {{\n  lib.shared({i})\n}}\n");
        fs::write(ws.join(format!("u{i}.silt")), text).unwrap();
    }
    let gate = dir.join("gate");
    fs::write(&gate, "").unwrap();

    let gate_text = gate.to_str().expect("utf8 path");
    let mut client = LspClient::spawn_with_root_and_env(
        Some(&path_to_uri(&ws)),
        &[("SILT_LSP_PRELOAD_GATE", gate_text)],
    );
    let uri = path_to_uri(&ws.join("lib.silt"));
    client.did_open_and_wait(&uri, lib);
    let id = crate::support::next_id();
    client.send_request(
        id,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 7 },
            "newName": "common",
        }),
    );
    // (The server reads the rename while the walk is held: a server
    // that answered it at once would answer it now, from lib.silt
    // alone.)
    std::thread::sleep(Duration::from_millis(300));
    fs::remove_file(&gate).unwrap();
    let resp = client.recv_until_within(Duration::from_secs(20), "the rename", |msg| {
        msg.get("id").and_then(|v| v.as_u64()) == Some(id)
    });
    let changes = resp["result"]["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("no changes: {resp}"));
    let mut files: Vec<&str> = changes
        .keys()
        .filter_map(|uri| uri.rsplit('/').next())
        .collect();
    files.sort();
    let mut expected: Vec<String> = (0..IMPORTERS).map(|i| format!("u{i}.silt")).collect();
    expected.push("lib.silt".to_string());
    expected.sort();
    assert_eq!(files, expected, "{resp}");

    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

/// A request that waits for the walk of the workspace ends when the
/// editor cancels it, and the server answers the next request.
#[cfg(debug_assertions)]
#[test]
fn a_cancel_ends_the_wait_for_the_preload() {
    use std::time::Duration;

    let dir = unique_tmp_dir("cancel_walking");
    let ws = dir.join("ws");
    fs::create_dir_all(&ws).unwrap();
    fs::write(ws.join("a.silt"), "fn a_unique_sym() { 0 }\n").unwrap();
    let gate = dir.join("gate");
    fs::write(&gate, "").unwrap();

    let gate_text = gate.to_str().expect("utf8 path");
    let mut client = LspClient::spawn_with_root_and_env(
        Some(&path_to_uri(&ws)),
        &[("SILT_LSP_PRELOAD_GATE", gate_text)],
    );
    let open = path_to_uri(&dir.join("open.silt"));
    client.did_open_and_wait(&open, "fn open_unique_sym() { 0 }\n");
    let id = crate::support::next_id();
    client.send_request(id, "workspace/symbol", json!({ "query": "unique_sym" }));
    client.send_notification("$/cancelRequest", json!({ "id": id }));
    let resp = client.recv_until_within(Duration::from_secs(20), "the cancelled answer", |msg| {
        msg.get("id").and_then(|v| v.as_u64()) == Some(id)
    });
    assert_eq!(resp["error"]["code"], -32800, "{resp}");
    let next = client.request_within(
        Duration::from_secs(20),
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": open } }),
    );
    assert!(next["result"].is_array(), "{next}");
    assert!(gate.exists(), "the walk is held");

    fs::remove_file(&gate).unwrap();
    client.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

/// A shutdown and an exit that come while a request waits for the walk
/// of the workspace end the wait: the waiting request is answered as
/// cancelled, the shutdown is answered, and the server stops at once.
#[cfg(debug_assertions)]
#[test]
fn a_shutdown_ends_the_wait_for_the_preload() {
    use std::time::{Duration, Instant};

    let dir = unique_tmp_dir("shutdown_walking");
    let ws = dir.join("ws");
    fs::create_dir_all(&ws).unwrap();
    fs::write(ws.join("a.silt"), "fn a_unique_sym() { 0 }\n").unwrap();
    let gate = dir.join("gate");
    fs::write(&gate, "").unwrap();

    let gate_text = gate.to_str().expect("utf8 path");
    let mut client = LspClient::spawn_with_root_and_env(
        Some(&path_to_uri(&ws)),
        &[("SILT_LSP_PRELOAD_GATE", gate_text)],
    );
    let open = path_to_uri(&dir.join("open.silt"));
    client.did_open_and_wait(&open, "fn open_unique_sym() { 0 }\n");
    let waiting = crate::support::next_id();
    client.send_request(
        waiting,
        "workspace/symbol",
        json!({ "query": "unique_sym" }),
    );
    let shutdown = crate::support::next_id();
    client.send_raw(&json!({ "jsonrpc": "2.0", "id": shutdown, "method": "shutdown" }));
    client.send_raw(&json!({ "jsonrpc": "2.0", "method": "exit" }));
    let start = Instant::now();
    let resp = client.recv_until_within(Duration::from_secs(20), "the waiting answer", |msg| {
        msg.get("id").and_then(|v| v.as_u64()) == Some(waiting)
    });
    assert_eq!(resp["error"]["code"], -32800, "{resp}");
    let resp = client.recv_until_within(Duration::from_secs(20), "the shutdown", |msg| {
        msg.get("id").and_then(|v| v.as_u64()) == Some(shutdown)
    });
    assert!(resp.get("error").is_none(), "{resp}");
    let exited = loop {
        if let Some(status) = client.child.try_wait().expect("try_wait") {
            break Some(status);
        }
        if start.elapsed() > Duration::from_secs(10) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(gate.exists(), "the walk is held");
    let _ = fs::remove_file(&gate);
    let _ = fs::remove_dir_all(&dir);
    assert!(
        exited.is_some_and(|status| status.success()),
        "the server did not stop: {exited:?}"
    );
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
