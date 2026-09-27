//! Round-101 GAP regression: LSP hover on a call's CALLEE identifier
//! must show the function's signature, not the call's RESULT type.
//!
//! Before the fix, the typechecker's named-callee shortcut (the
//! `ExprKind::Call` / `ExprKind::Pipe` arms in
//! `src/typechecker/inference.rs`) looked the callee scheme up directly
//! and never stashed `callee.ty`, so the LSP expression walk fell back
//! to the enclosing Call node's result type: hover on `add` in
//! `let r = add(1, 2)` rendered a signature block of `Int` (while the
//! effects/doc blocks in the same hover described the FUNCTION), and
//! hover on `println` rendered a bare `()`. Qualified callees
//! (`list.sum`) already stashed the instantiated fn type — this fix
//! makes bare callees consistent with them.
//!
//! Mirrors the harness in `tests/lsp_hover_fn_decl_tests.rs`.

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
        .pointer("/contents/value")
        .and_then(|v| v.as_str())
        .expect("hover.contents.value is a string")
        .to_string()
}

// Shared source under test:
//   line 0: fn add(a: Int, b: Int) -> Int { a + b }
//   line 1: fn main() {
//   line 2:   let r = add(1, 2)
//   line 3:   println(r)
//   line 4: }
const SOURCE: &str =
    "fn add(a: Int, b: Int) -> Int { a + b }\nfn main() {\n  let r = add(1, 2)\n  println(r)\n}\n";

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn hover_on_user_fn_callee_shows_signature() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_callee_user_fn.silt";
    client.did_open_and_wait(uri, SOURCE);

    // Cursor on `add` in `let r = add(1, 2)` (line 2, `add` spans
    // characters 10..13).
    let value = hover_value(&mut client, uri, 2, 11);
    assert!(
        value.contains("Fn(Int, Int) -> Int"),
        "hover on the callee `add` must show the fn signature \
         `Fn(Int, Int) -> Int`, not the call's result type; got {value:?}"
    );
    assert!(
        !value.contains("```silt\nInt\n```"),
        "hover on the callee `add` must not render the call RESULT type \
         as the signature block; got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_println_callee_is_not_bare_unit() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_callee_println.silt";
    client.did_open_and_wait(uri, SOURCE);

    // Cursor on `println` in `println(r)` (line 3, `println` spans
    // characters 2..9).
    let value = hover_value(&mut client, uri, 3, 4);
    assert!(
        !value.contains("```silt\n()\n```"),
        "hover on the callee `println` must not render a bare `()` \
         signature block (the call's result type); got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_let_binder_still_shows_call_result() {
    // Control: the whole-call result position (the `let` binder) keeps
    // showing the call's result type, not the callee's signature.
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_callee_control_binder.silt";
    client.did_open_and_wait(uri, SOURCE);

    // Cursor on `r` in `let r = add(1, 2)` (line 2, character 6).
    let value = hover_value(&mut client, uri, 2, 6);
    assert!(
        value.contains("Int"),
        "hover on the let binder must show the call result `Int`; got {value:?}"
    );
    assert!(
        !value.contains("Fn("),
        "hover on the let binder must not show the callee signature; got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_piped_callee_shows_signature() {
    // The Pipe arm has the same named-callee shortcut as the Call arm;
    // lock it too.
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_hover_callee_piped.silt";
    let source = "fn add(a: Int, b: Int) -> Int { a + b }\nfn main() {\n  let s = 1 |> add(2)\n  println(s)\n}\n";
    client.did_open_and_wait(uri, source);

    // Cursor on `add` in `1 |> add(2)` (line 2, `add` spans
    // characters 15..18).
    let value = hover_value(&mut client, uri, 2, 16);
    assert!(
        value.contains("Fn(Int, Int) -> Int"),
        "hover on a piped callee must show the fn signature; got {value:?}"
    );
    client.shutdown();
}
