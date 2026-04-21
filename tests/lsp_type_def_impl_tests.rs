//! End-to-end LSP tests for `textDocument/typeDefinition` and
//! `textDocument/implementation`.
//!
//! Mirrors the harness in tests/lsp_workspace_tests.rs — spawns
//! `silt lsp` as a subprocess and speaks LSP JSON-RPC over stdio.

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
fn type_definition_jumps_to_user_type() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_typedef_point.silt";
    // Line 0: `type Point { x: Int, y: Int }`
    // Line 1: (blank)
    // Line 2: `fn main() { let p = Point { x: 1, y: 2 } p }`
    let src = "type Point { x: Int, y: Int }\n\nfn main() { let p = Point { x: 1, y: 2 } p }\n";
    client.did_open_and_wait(file, src);

    // Click on the trailing `p` at the end of main's body. The `p` we
    // land on sits right before the closing `}`. Its inferred type is
    // the record `Point`, so typeDefinition should jump to the type
    // decl on line 0.
    let line2 = "fn main() { let p = Point { x: 1, y: 2 } p }";
    let p_col = line2.rfind(" p ").unwrap() + 1; // index of the trailing `p`
    let resp = client.request(
        "textDocument/typeDefinition",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 2, "character": p_col }
        }),
    );
    let result = resp
        .get("result")
        .unwrap_or_else(|| panic!("expected typeDefinition result; got {resp}"));
    assert!(
        !result.is_null(),
        "typeDefinition should not be null; got {resp}"
    );
    let location = match result {
        Value::Object(_) => result.clone(),
        Value::Array(arr) if !arr.is_empty() => arr[0].clone(),
        _ => panic!("unexpected result shape: {result}"),
    };
    assert_eq!(
        location.get("uri").and_then(|v| v.as_str()),
        Some(file),
        "type definition should live in the same file; got {location}"
    );
    let start_line = location
        .pointer("/range/start/line")
        .and_then(|v| v.as_u64())
        .expect("range.start.line");
    assert_eq!(
        start_line, 0,
        "Point's declaration is on line 0; got line {start_line}"
    );
    client.shutdown();
}

#[test]
fn implementation_lists_trait_impls() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_impl_foo.silt";
    // A trait `Foo` with two impls. We click on the `Foo` reference
    // inside the first `trait Foo for A` header (which is itself a
    // trait_name ident reference recognised by find_ident_at_offset
    // via the surrounding program). The handler walks every open doc's
    // decls so the two impls are returned.
    let src = "\
trait Foo { fn m(self) -> Int }
type A { n: Int }
type B { n: Int }
trait Foo for A { fn m(self) -> Int { 1 } }
trait Foo for B { fn m(self) -> Int { 2 } }
fn caller() { Foo }
";
    client.did_open_and_wait(file, src);

    // The last line, `fn caller() { Foo }`, places `Foo` as an Ident
    // expression inside a function body — exactly where
    // `find_ident_at_offset` can recognise it. Column of `Foo` on
    // line 5 is 14 (0-based). Any column inside the 3-letter name works.
    let resp = client.request(
        "textDocument/implementation",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 5, "character": 15 }
        }),
    );
    let result = resp
        .get("result")
        .unwrap_or_else(|| panic!("expected implementation result; got {resp}"));
    let arr = match result {
        Value::Array(arr) => arr.clone(),
        Value::Object(_) => vec![result.clone()],
        _ => panic!("unexpected result shape: {result}"),
    };
    assert_eq!(
        arr.len(),
        2,
        "expected 2 trait impls for Foo; got {} — {arr:?}",
        arr.len()
    );
    for loc in &arr {
        assert_eq!(
            loc.get("uri").and_then(|v| v.as_str()),
            Some(file),
            "impl location should be in the opened file; got {loc}"
        );
    }
    client.shutdown();
}
