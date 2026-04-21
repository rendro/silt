//! Tier 2 LSP features: inlay hints, document highlight, folding
//! range, selection range.
//!
//! Shares the subprocess harness shape with tests/lsp.rs and
//! tests/lsp_workspace_tests.rs.

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
                Err(RecvTimeoutError::Timeout) => panic!("timeout id={id}"),
                Err(RecvTimeoutError::Disconnected) => panic!("disconnected id={id}"),
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
                panic!("diagnostic timeout for {uri}");
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
                Err(_) => panic!("diagnostic recv error"),
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
fn inlay_hints_shows_inferred_types() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_t2_inlay.silt";
    let src = "fn main() {\n  let x = 42\n  let s = \"hi\"\n  x\n}\n";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": file },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 5, "character": 0 }
            }
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("inlay hint result array");
    let labels: Vec<String> = arr
        .iter()
        .filter_map(|h| h.get("label").and_then(|l| l.as_str()).map(String::from))
        .collect();
    assert!(
        labels.iter().any(|l| l == ": Int"),
        "expected `: Int` hint; got {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == ": String"),
        "expected `: String` hint; got {labels:?}"
    );
    client.shutdown();
}

#[test]
fn document_highlight_returns_all_ident_occurrences() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_t2_hl.silt";
    // `count` appears three times in the body.
    let src = "fn main() {\n  let count = 1\n  let y = count + count\n  y\n}\n";
    client.did_open_and_wait(file, src);

    // Cursor on the first `count` use at line 2.
    let resp = client.request(
        "textDocument/documentHighlight",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 2, "character": 10 }
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("highlight result");
    assert!(
        arr.len() >= 2,
        "expected at least two highlights, got {arr:?}"
    );
    client.shutdown();
}

#[test]
fn folding_range_covers_fn_body() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_t2_fold.silt";
    let src = "fn main() {\n  let x = 1\n  let y = 2\n  x + y\n}\n";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": file } }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("folding range result");
    assert!(!arr.is_empty(), "expected at least one fold range");
    // At least one fold should start at line 0 (the fn decl).
    let has_fn_fold = arr.iter().any(|f| {
        f.get("startLine").and_then(|l| l.as_u64()) == Some(0)
            && f.get("endLine")
                .and_then(|l| l.as_u64())
                .map(|l| l > 0)
                .unwrap_or(false)
    });
    assert!(has_fn_fold, "expected a fold covering the fn body; got {arr:?}");
    client.shutdown();
}

#[test]
fn selection_range_returns_nested_chain() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_t2_sel.silt";
    let src = "fn main() {\n  1 + 2\n}\n";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": file },
            "positions": [ { "line": 1, "character": 2 } ]
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("selection range result");
    assert_eq!(arr.len(), 1);
    let first = &arr[0];
    // The response should have a nested `parent` somewhere.
    let has_parent = first.get("parent").is_some();
    assert!(
        has_parent,
        "expected a selection range with a parent; got {first:?}"
    );
    client.shutdown();
}
