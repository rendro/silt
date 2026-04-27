//! Round-60 B8 + G4 regression: LSP rename / prepareRename must work
//! when the cursor sits on a *binding* site (the LHS of a `let`, a
//! `fn` parameter pattern, or a `fn` declaration name) — not only on a
//! use-site.
//!
//! Before the fix, `find_ident_at_offset` walked only `ExprKind::Ident`
//! nodes, so:
//!   * `prepareRename` on a binder returned `null`
//!   * `rename` on a binder returned `null`
//!
//! Even though the references collector in `workspace.rs` already
//! covered binding sites once the symbol was known, the initial cursor
//! lookup never produced a symbol from a binder. This test locks the
//! end-to-end LSP behaviour via the stdio transport.
//!
//! Mirrors the harness in `tests/lsp_workspace_tests.rs`.

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
fn rename_from_let_binding_site() {
    // Source layout — cursor on `xvar` binder of `let xvar = 42`:
    //   line 0: `fn main() {`
    //   line 1: `  let xvar = 42`
    //                ^^^^ starts at char=6
    //   line 2: `  println(xvar)`
    //   line 3: `}`
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_let_binder.silt";
    client.did_open_and_wait(uri, "fn main() {\n  let xvar = 42\n  println(xvar)\n}\n");

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 6 },
            "newName": "renamed_xvar"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on let binder must NOT return null (round-60 B8); got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    assert!(
        edits.len() >= 2,
        "expected at least 2 edits (binder + use); got {}: {edits:?}",
        edits.len()
    );
    client.shutdown();
}

#[test]
fn prepare_rename_from_let_binding_site() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_pr_let_binder.silt";
    client.did_open_and_wait(uri, "fn main() {\n  let xvar = 42\n  println(xvar)\n}\n");

    let resp = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 6 }
        }),
    );
    let result = resp.get("result").expect("prepareRename has result");
    assert!(
        !result.is_null(),
        "prepareRename on let binder must NOT return null (round-60 B8); got {resp}"
    );
    client.shutdown();
}

#[test]
fn rename_from_fn_param_binding_site() {
    // `fn add(x, y) { x + y }` — cursor on `x` parameter binder.
    // Line 0: `fn add(x, y) { x + y }`
    //          0123456789012
    // `x` at char=7
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_fn_param.silt";
    client.did_open_and_wait(uri, "fn add(x, y) { x + y }\n");

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 7 },
            "newName": "renamed_x"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on fn-param binder must NOT return null (round-60 B8); got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    assert!(
        edits.len() >= 2,
        "expected at least 2 edits (binder + use); got {}: {edits:?}",
        edits.len()
    );
    client.shutdown();
}

#[test]
fn rename_from_fn_decl_name() {
    // `fn helper() { 0 }\nfn main() { helper() }` — cursor on `helper`
    // declaration name. Line 0: `fn helper() { 0 }`
    //                            0123456789
    // `helper` at char=3
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_fn_decl_name.silt";
    client.did_open_and_wait(uri, "fn helper() { 0 }\nfn main() { helper() }\n");

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
            "newName": "renamed_helper"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on fn-decl name binder must NOT return null (round-60 B8); got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    assert!(
        edits.len() >= 2,
        "expected at least 2 edits (decl + call); got {}: {edits:?}",
        edits.len()
    );
    client.shutdown();
}

#[test]
fn rename_from_use_site_still_works() {
    // Positive guard — the existing use-site path must not regress.
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_use_site_guard.silt";
    client.did_open_and_wait(uri, "fn helper() { 0 }\nfn main() { helper() }\n");

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            // Cursor on `helper()` call: line 1, the `h` is at char=12.
            "position": { "line": 1, "character": 12 },
            "newName": "fresh"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename from use-site must continue to work; got {resp}"
    );
    client.shutdown();
}
