//! End-to-end LSP tests for Tier 1 workspace features:
//! cross-file goto-def, references, rename, workspace/symbol.
//!
//! Mirrors the harness in tests/lsp.rs — spawns `silt lsp` as a
//! subprocess and speaks LSP JSON-RPC over stdio.

use std::io::{BufRead, BufReader, Read, Write};
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
    fn spawn() -> Self {
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
        client.initialize();
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

    fn initialize(&mut self) {
        let id = next_id();
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": { "capabilities": {} }
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
        Value::Array(arr) if !arr.is_empty() => arr[0]
            .get("uri")
            .and_then(|v| v.as_str())
            .map(String::from),
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
