//! Round-60 G4 regression: LSP hover on a `fn` declaration name (the
//! binder, not a call site) must return the function's signature, not
//! `null`.
//!
//! Before the fix, `find_ident_at_offset` walked only `ExprKind::Ident`
//! nodes and never matched the `fn foo` declaration name, so hover at
//! the binder fell through to a no-type response.
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
fn hover_on_fn_decl_name() {
    // Source: `fn helper() { 0 }`
    //          0123456789
    // Cursor on `helper` at char=3.
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_fn_decl_name.silt";
    client.did_open_and_wait(uri, "fn helper() { 0 }\nfn main() { helper() }\n");

    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 }
        }),
    );
    let result = resp.get("result").expect("hover has result");
    assert!(
        !result.is_null(),
        "hover on fn decl name must NOT be null (round-60 G4); got {resp}"
    );
    // Hover content should mention a type — for a 0-arg Int-returning fn
    // the signature pretty-printer renders as something containing
    // either `Int` (return type) or `()` (params). We just assert the
    // markup is non-empty and contains a `silt` code fence.
    let contents = result
        .get("contents")
        .expect("hover.result.contents is present");
    let value_str = contents
        .get("value")
        .and_then(|v| v.as_str())
        .expect("hover.result.contents.value is a string");
    assert!(
        value_str.contains("silt"),
        "expected hover markdown to contain a `silt` code fence; got {value_str:?}"
    );
    assert!(
        !value_str.trim().is_empty(),
        "hover markdown must not be empty"
    );
    client.shutdown();
}

#[test]
fn hover_on_fn_decl_name_renders_signature_substring() {
    // `fn add(x: Int, y: Int): Int { x + y }` — fully-annotated so the
    // typechecker resolves all type variables, and hover on `add`
    // produces a non-empty type with no `Var(_)` placeholders. Without
    // the round-60 fix, `find_ident_at_offset` would not match the
    // `add` binder and hover would return null.
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_fn_decl_sig.silt";
    client.did_open_and_wait(
        uri,
        "fn add(x: Int, y: Int) -> Int { x + y }\nfn main() { add(1, 2) }\n",
    );

    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 }
        }),
    );
    let result = resp.get("result").expect("hover has result");
    assert!(
        !result.is_null(),
        "hover on fn decl name must NOT be null; got {resp}"
    );
    let contents_value = result
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .expect("hover contents.value is a string");
    // Pre-fix the assertion accepted any hover containing `->` OR `Int`,
    // which would pass even if hover returned just `Int`. Tighten: require
    // the return-type arrow AND a parameter shape — either the pretty
    // `(Int, Int)` form or the raw `fn add` declaration prefix.
    assert!(
        contents_value.contains("->"),
        "hover did not render return-type arrow; got {contents_value:?}"
    );
    assert!(
        contents_value.contains("(Int, Int)") || contents_value.contains("fn add"),
        "hover did not render signature parameters or fn decl; got {contents_value:?}"
    );
    client.shutdown();
}
