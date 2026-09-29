//! Regression test for round-23 fix agent E:
//! LSP hover on chained field access must return the *field's* type at
//! every depth, not just the first dot.
//!
//! The typechecker annotates intermediate field-access nodes with
//! `Type::Generic(<record_name>, [])` (see
//! `typechecker::inference::type_from_name`) rather than the fully
//! expanded `Type::Record(...)`. Prior to the fix, `get_field_type` in
//! `src/lsp.rs` only handled `Type::Record` / `Type::Tuple`, so hovering
//! any field past the leftmost dot (e.g. `o.inner.val`) silently fell
//! back to the receiver's own type.
//!
//! This test drives the real parse + typecheck + LSP hover pipeline via
//! the stdio transport, exactly as a client (VS Code etc.) would, so it
//! locks the behaviour end-to-end — the synthetic-Type::Record unit
//! tests in src/lsp.rs cannot catch this regression because they never
//! run the typechecker.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
static URI_COUNTER: AtomicU64 = AtomicU64::new(1);

const READ_TIMEOUT: Duration = Duration::from_secs(10);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_chained_hover_{n}.silt")
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
            .expect("failed to spawn silt lsp");

        let stdin = child.stdin.take().expect("no stdin on child");
        let stdout = child.stdout.take().expect("no stdout on child");

        let (tx, rx) = channel::<ServerMessage>();
        thread::spawn(move || reader_loop(stdout, tx));

        LspClient { child, stdin, rx }
    }

    fn send_raw(&mut self, msg: &Value) {
        let body = serde_json::to_string(msg).expect("serialize");
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        self.stdin
            .write_all(framed.as_bytes())
            .expect("write to child stdin");
        self.stdin.flush().expect("flush child stdin");
    }

    fn send_request(&mut self, id: u64, method: &str, params: Value) {
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
    }

    fn send_notification(&mut self, method: &str, params: Value) {
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    fn recv_response_for(&self, id: u64) -> ServerMessage {
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
                    panic!("silt lsp server closed its stdout unexpectedly");
                }
            }
        }
    }

    fn initialize(&mut self) {
        let id = next_id();
        self.send_request(
            id,
            "initialize",
            json!({
                "processId": null,
                "rootUri": null,
                "capabilities": {},
            }),
        );
        let _ = self.recv_response_for(id);
        self.send_notification("initialized", json!({}));
    }

    fn did_open_and_wait(&mut self, uri: &str, source: &str) {
        self.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "silt",
                    "version": 1,
                    "text": source,
                }
            }),
        );
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
                    panic!("timed out waiting for publishDiagnostics for {uri}");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("silt lsp server closed its stdout unexpectedly");
                }
            }
        }
    }

    fn shutdown(mut self) {
        let id = next_id();
        self.send_request(id, "shutdown", json!(null));
        let _ = self.rx.recv_timeout(READ_TIMEOUT);
        self.send_notification("exit", json!(null));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
                Ok(None) => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
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

fn hover_value_at(client: &mut LspClient, uri: &str, line: u32, character: u32) -> String {
    let id = next_id();
    client.send_request(
        id,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    let resp = client.recv_response_for(id);
    assert!(
        resp.get("error").is_none(),
        "hover request returned an error: {resp}"
    );
    let result = resp
        .get("result")
        .expect("hover response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null hover result, got: {resp}"
    );
    result
        .pointer("/contents/value")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("hover result must contain contents.value string: {result}"))
        .to_string()
}

// ── Tests ──────────────────────────────────────────────────────────

// Source under test for the two following chained-hover tests:
//
//   line 0: type Inner { val: Int }
//   line 1: type Outer { inner: Inner }
//   line 2: fn main() {
//   line 3:   let o = Outer { inner: Inner { val: 42 } }
//   line 4:   println(o.inner.val)
//   line 5: }
//
// For line 4 — "  println(o.inner.val)":
//   0         1         2
//   012345678901234567890123456
//            1111111111
// Columns of interest:
//   col 10: 'o'
//   col 12: 'i' of `inner`
//   col 18: 'v' of `val`
const CHAINED_SOURCE: &str = "type Inner { val: Int }\n\
type Outer { inner: Inner }\n\
fn main() {\n  \
let o = Outer { inner: Inner { val: 42 } }\n  \
println(o.inner.val)\n\
}\n";

#[test]
fn test_hover_on_rightmost_field_of_chain_returns_field_type() {
    // GAP (round-23 fix agent E): hover on the deepest field of a chained
    // access must return `Int` (the field's type), not the outer record's
    // type.  Prior to the fix, `get_field_type` handled only `Type::Record`
    // and `Type::Tuple`, so the intermediate `Type::Generic("Inner", [])`
    // fell through and the hover reported `Outer {...}` instead.
    let mut client = LspClient::spawn();
    client.initialize();

    let uri = unique_uri();
    client.did_open_and_wait(&uri, CHAINED_SOURCE);

    // Hover at column 18 (the 'v' of `val` in "  println(o.inner.val)")
    let value = hover_value_at(&mut client, &uri, 4, 18);

    // The field hover must mention the field name AND its type `Int`,
    // and must NOT mistakenly surface the outer record.
    assert!(
        value.contains("val"),
        "hover on o.inner.val must mention field name `val`, got: {value}"
    );
    assert!(
        value.contains("Int"),
        "hover on o.inner.val must resolve field type to `Int`, got: {value}"
    );
    assert!(
        !value.contains("Outer"),
        "hover on the rightmost field must NOT report the outer record type, got: {value}"
    );

    client.shutdown();
}

#[test]
fn test_hover_on_middle_field_of_chain_returns_record_type() {
    // Hover on the *middle* field (`inner`) of the same chain should
    // resolve to the `Inner` record — not to `Outer`, and not to
    // `Int` (which is only the leaf's type).  This pins the
    // `get_field_type_resolved` path that looks up a named record via
    // the program's type declarations.
    let mut client = LspClient::spawn();
    client.initialize();

    let uri = unique_uri();
    client.did_open_and_wait(&uri, CHAINED_SOURCE);

    // Hover at column 12 (the 'i' of `inner` in "  println(o.inner.val)")
    let value = hover_value_at(&mut client, &uri, 4, 12);

    assert!(
        value.contains("inner"),
        "hover on o.inner must mention field name `inner`, got: {value}"
    );
    assert!(
        value.contains("Inner"),
        "hover on o.inner must resolve its type to the `Inner` record, got: {value}"
    );

    client.shutdown();
}
