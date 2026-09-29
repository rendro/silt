//! LSP regression tests for dot-completion on bindings whose typechecked
//! type is `Type::AnonRecord { .. }`.
//!
//! Before the fix, `record_fields_from_type` (src/lsp/fields.rs) only
//! matched `Type::Record` and `Type::Generic`, so a `let p: Point = { x:
//! 1, y: 2 }` (where the typechecker assigns `Type::AnonRecord` to `p`)
//! produced an empty completion list when the user requested `p.|`.
//! These tests exercise the LSP via the subprocess JSON-RPC scaffold —
//! the bug lives in the LSP path, not the typechecker, so unit tests on
//! `record_fields_from_type` alone wouldn't catch a regression here.
//!
//! The two scenarios:
//!   1. Named type annotation with a record-literal initializer
//!      (`let p: Point = { x: 1, y: 2 }`).
//!   2. Pure anonymous-record binding (`let p = { x: 1, y: 2 }`).

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
const READ_TIMEOUT: Duration = Duration::from_secs(15);

// `CompletionItemKind::FIELD` = 5 (LSP 3.17 spec).  We compare numerics
// in JSON rather than pulling in the `lsp_types` crate just for one enum.
const COMPLETION_ITEM_KIND_FIELD: u64 = 5;

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri(tag: &str) -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_anon_field_{tag}_{n}.silt")
}

type ServerMessage = Value;

struct LspClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<ServerMessage>,
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
        let (tx, rx) = channel::<ServerMessage>();
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

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn reader_loop<R: Read + Send + 'static>(stdout: R, tx: Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
            if line == "\r\n" || line == "\n" || line.is_empty() {
                break;
            }
            if let Some(rest) = line
                .strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
                && let Ok(n) = rest.trim().parse::<usize>()
            {
                content_length = Some(n);
            }
        }
        let Some(n) = content_length else {
            return;
        };
        let mut body = vec![0u8; n];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(val) = serde_json::from_slice::<Value>(&body) else {
            return;
        };
        if tx.send(val).is_err() {
            return;
        }
    }
}

/// Pull `(label, kind)` pairs out of either the bare-array or
/// `CompletionList` response shapes.
fn extract_completion_items(result: &Value) -> Vec<(String, Option<u64>)> {
    let arr = if let Some(arr) = result.as_array() {
        arr.clone()
    } else if let Some(arr) = result.pointer("/items").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|it| {
            let label = it.get("label").and_then(|l| l.as_str())?.to_string();
            let kind = it.get("kind").and_then(|k| k.as_u64());
            Some((label, kind))
        })
        .collect()
}

fn assert_field(items: &[(String, Option<u64>)], expected_label: &str, resp: &Value) {
    let found = items.iter().find(|(l, _)| l == expected_label);
    let Some((_, kind)) = found else {
        panic!(
            "completion missing field `{expected_label}`; got {} items: {:?}; full resp: {}",
            items.len(),
            items.iter().map(|(l, _)| l).collect::<Vec<_>>(),
            resp
        );
    };
    assert_eq!(
        *kind,
        Some(COMPLETION_ITEM_KIND_FIELD),
        "completion item `{expected_label}` must have kind FIELD ({COMPLETION_ITEM_KIND_FIELD}); \
         got kind={kind:?}; full resp: {resp}"
    );
}

// ── Field completion via named record annotation ───────────────────

#[test]
fn field_completion_on_user_record_via_anon_type_returns_fields() {
    // Source layout:
    //   line 0: `type Point = { x: Int, y: Int }`
    //   line 1: `fn main() {`
    //   line 2: `  let p: Point = { x: 1, y: 2 }`
    //   line 3: `  println(p.x)`
    //   line 4: `}`
    //
    // The typechecker assigns `Type::AnonRecord { .. }` (not
    // `Type::Record(Point, ...)`) to `p` because the initialiser is a
    // bare record literal. Before the fix, `record_fields_from_type`
    // only handled `Type::Record` / `Type::Generic`, so dot-completion
    // on `p.|x` fell through and returned zero items.
    //
    // Cursor: line 3, character = 12 — the column just before the `x`
    // in `println(p.x)` (after the `.`):
    //
    //   `  println(p.x)`
    //    0         1
    //    0123456789012
    //              ^ char=12 is the position of `x`; trigger char `.`
    let mut client = LspClient::spawn();
    let uri = unique_uri("named_anon_record");
    let text = "type Point = { x: Int, y: Int }\nfn main() {\n  let p: Point = { x: 1, y: 2 }\n  println(p.x)\n}\n";
    client.did_open_and_wait(&uri, text);

    let resp = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 3, "character": 12 },
            "context": { "triggerKind": 2, "triggerCharacter": "." }
        }),
    );
    let result = resp.get("result").expect("completion has result");
    let items = extract_completion_items(result);
    assert!(
        !items.is_empty(),
        "completion on `p.|x` must NOT return zero items for a user record \
         (regression: anon-record blind spot in record_fields_from_type); resp={resp}"
    );
    assert_field(&items, "x", &resp);
    assert_field(&items, "y", &resp);
    client.shutdown();
}

// ── Field completion via pure anonymous record ─────────────────────

#[test]
fn field_completion_on_anon_record_literal_returns_fields() {
    // Source layout (no named type — the binder is purely structural):
    //   line 0: `fn main() {`
    //   line 1: `  let p = { x: 1, y: 2 }`
    //   line 2: `  println(p.x)`
    //   line 3: `}`
    //
    // Cursor on the `x` in `p.x` (line 2, character=12):
    //   `  println(p.x)`
    //    0         1
    //    0123456789012
    //              ^ char=12
    let mut client = LspClient::spawn();
    let uri = unique_uri("pure_anon_record");
    let text = "fn main() {\n  let p = { x: 1, y: 2 }\n  println(p.x)\n}\n";
    client.did_open_and_wait(&uri, text);

    let resp = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 12 },
            "context": { "triggerKind": 2, "triggerCharacter": "." }
        }),
    );
    let result = resp.get("result").expect("completion has result");
    let items = extract_completion_items(result);
    assert!(
        !items.is_empty(),
        "completion on `p.|x` must NOT return zero items for an anon-record \
         binding (regression: anon-record blind spot in \
         record_fields_from_type); resp={resp}"
    );
    assert_field(&items, "x", &resp);
    assert_field(&items, "y", &resp);
    client.shutdown();
}
