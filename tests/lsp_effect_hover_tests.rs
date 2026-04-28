//! Phase B locks for the LSP hover renderer's effect-set output.
//!
//! See `docs/proposals/effect-rows.md` (Part 7). On hover over a fn
//! decl, the LSP must emit the effect annotation between the type
//! signature and the doc-comment separator. Three render variants:
//!   - declared `!{io, fs}` → `effects: !{io, fs}`
//!   - no annotation → `effects: !*` (loud, signals gradual rollout)
//!   - declared narrower than body, or body narrower than declared →
//!     two lines (`effects: ... (declared)` / `inferred: ... (body)`).
//!
//! Mirrors the harness used by `tests/lsp_hover_fn_decl_tests.rs` —
//! drives the real silt-lsp binary over stdio so the rendering is
//! locked end-to-end.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
const READ_TIMEOUT: Duration = Duration::from_secs(15);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_effect_hover_{n}.silt")
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

fn hover_value(client: &mut LspClient, uri: &str, line: u32, character: u32) -> String {
    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    let result = resp.get("result").expect("hover has result");
    assert!(!result.is_null(), "hover must not be null; got {resp}");
    result
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .expect("hover.contents.value is a string")
}

// ── 1. Hover renders declared effects ──────────────────────────────

#[test]
fn hover_renders_declared_effects() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    // Pure body so inferred == EMPTY; declared == {io, fs}; the two
    // differ, so this ALSO exercises the dual-render path. Test 3
    // locks the dual-render content; here we just need the declared
    // line to be present.
    client.did_open_and_wait(&uri, "fn read() -> Int !{io, fs} = 0\n");
    let value = hover_value(&mut client, &uri, 0, 3); // cursor on `read`
    assert!(
        value.contains("effects:"),
        "hover must include an effects: line; got {value:?}"
    );
    assert!(
        value.contains("!{fs, io}"),
        "hover must render declared effects in alphabetic order; got {value:?}"
    );
    client.shutdown();
}

// ── 2. Hover renders TOP for unannotated fn ────────────────────────

#[test]
fn hover_renders_top_for_unannotated() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    // No annotation → declared defaults to TOP. Hover should loudly
    // surface `!*` so users see the gradual-rollout state.
    client.did_open_and_wait(&uri, "fn legacy() -> Int = 0\n");
    let value = hover_value(&mut client, &uri, 0, 3); // cursor on `legacy`
    assert!(
        value.contains("effects: !*"),
        "unannotated fn must render `effects: !*`; got {value:?}"
    );
    client.shutdown();
}

// ── 3. Dual-render when declared and inferred differ ───────────────

#[test]
fn hover_renders_inferred_when_narrower_than_declared() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    // Declared `!{io}` but body is fully pure (inferred EMPTY).
    // The two differ → both lines appear in the hover render.
    client.did_open_and_wait(&uri, "fn pretend() -> Int !{io} = 0\n");
    let value = hover_value(&mut client, &uri, 0, 3); // cursor on `pretend`
    assert!(
        value.contains("effects: !{io}"),
        "hover must show declared `!{{io}}`; got {value:?}"
    );
    assert!(
        value.contains("(declared)"),
        "hover must label the declared line; got {value:?}"
    );
    assert!(
        value.contains("inferred: !{}"),
        "hover must show inferred `!{{}}` when body is pure; got {value:?}"
    );
    assert!(
        value.contains("(body)"),
        "hover must label the inferred line; got {value:?}"
    );
    client.shutdown();
}
