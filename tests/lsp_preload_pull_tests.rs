//! End-to-end tests for two LSP features:
//!   - Workspace preload on initialize (indexes `.silt` files the
//!     editor has not yet opened).
//!   - Pull-model diagnostics (`textDocument/diagnostic`).
//!
//! Harness mirrors `tests/lsp_workspace_tests.rs`: spawns `silt lsp`
//! as a subprocess and speaks LSP JSON-RPC over stdio.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
const READ_TIMEOUT: Duration = Duration::from_secs(15);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn reader_loop(stdout: std::process::ChildStdout, tx: std::sync::mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut header = String::new();
        let mut content_length: Option<usize> = None;
        loop {
            header.clear();
            match reader.read_line(&mut header) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
            if header == "\r\n" || header == "\n" {
                break;
            }
            if let Some(rest) = header.trim_end().strip_prefix("Content-Length:") {
                content_length = rest.trim().parse().ok();
            }
        }
        let Some(len) = content_length else { return };
        let mut buf = vec![0u8; len];
        if reader.read_exact(&mut buf).is_err() {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&buf) else {
            return;
        };
        if tx.send(value).is_err() {
            return;
        }
    }
}

struct LspClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
}

impl LspClient {
    /// Spawn with an explicit `rootUri` so the server knows where to preload.
    fn spawn_with_root(root_uri: Option<&str>) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn silt lsp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = channel::<Value>();
        thread::spawn(move || reader_loop(stdout, tx));
        let mut client = LspClient { child, stdin, rx };
        client.initialize(root_uri);
        client
    }

    fn send_raw(&mut self, msg: &Value) {
        let body = serde_json::to_string(msg).unwrap();
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        self.stdin.write_all(framed.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv_response_for(&self, id: u64) -> Value {
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::from_millis(0));
            if remaining.is_zero() {
                panic!("timed out waiting for response id={id}");
            }
            match self.rx.recv_timeout(remaining) {
                Ok(msg) => {
                    if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        return msg;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("timed out waiting for response id={id}");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("server disconnected waiting for id={id}");
                }
            }
        }
    }

    fn initialize(&mut self, root_uri: Option<&str>) {
        let id = next_id();
        let mut params = json!({ "capabilities": {} });
        if let Some(r) = root_uri {
            params["rootUri"] = json!(r);
        }
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": params
        }));
        let _ = self.recv_response_for(id);
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }));
    }

    fn did_open_and_wait(&mut self, uri: &str, text: &str) {
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "silt",
                    "version": 1,
                    "text": text
                }
            }
        }));
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::from_millis(0));
            if remaining.is_zero() {
                panic!("timed out waiting for publishDiagnostics for {uri}");
            }
            match self.rx.recv_timeout(remaining) {
                Ok(msg) => {
                    if msg.get("id").is_none()
                        && msg.get("method").and_then(|v| v.as_str())
                            == Some("textDocument/publishDiagnostics")
                        && msg.pointer("/params/uri").and_then(|v| v.as_str()) == Some(uri)
                    {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("diagnostic timeout for {uri}");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("server disconnected");
                }
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = next_id();
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        self.recv_response_for(id)
    }

    fn shutdown(mut self) {
        let id = next_id();
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "shutdown"
        }));
        let _ = self.rx.recv_timeout(READ_TIMEOUT);
        self.send_raw(&json!({"jsonrpc": "2.0", "method": "exit"}));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => return,
            }
        }
    }
}

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
