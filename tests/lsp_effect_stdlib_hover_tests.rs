//! Phase C: hover on a stdlib call (`io.read_file`, `tcp.connect`,
//! `list.map`, …) must surface the builtin's effect set in the same
//! `effects: !{...}` block the user-fn hover uses. The lookup path
//! goes through `Server::builtin_effects`, populated from
//! `typechecker::builtin_effects()`.
//!
//! See `docs/proposals/effect-rows.md` Part 7 for the rollout plan.
//! Mirrors the harness used by `tests/lsp_effect_hover_tests.rs`.

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
    format!("file:///tmp/silt_lsp_effect_stdlib_hover_{n}.silt")
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

#[test]
fn hover_on_io_read_file_shows_io_fs_effects() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    // Place the call inside a body so the source has a clean cursor
    // anchor. The `read_file` token starts at column 19 on line 2.
    let src = "import io\nfn main() {\n  io.read_file(\"x\")\n}\n";
    client.did_open_and_wait(&uri, src);
    // Cursor on `read_file` (column ~6 in `  io.read_file(`).
    let value = hover_value(&mut client, &uri, 2, 8);
    assert!(
        value.contains("effects: !{fs, io}"),
        "hover on io.read_file must render `effects: !{{fs, io}}`; got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_list_map_shows_pure_effects() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let src = "import list\nfn main() {\n  list.map([1, 2], fn(x) { x })\n}\n";
    client.did_open_and_wait(&uri, src);
    // Cursor on `map`.
    let value = hover_value(&mut client, &uri, 2, 8);
    assert!(
        value.contains("effects: !{}"),
        "hover on list.map must render `effects: !{{}}` (pure); got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_println_shows_io_effects() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let src = "fn main() {\n  println(\"hi\")\n}\n";
    client.did_open_and_wait(&uri, src);
    // Cursor on `println` (the global, not module-qualified).
    let value = hover_value(&mut client, &uri, 1, 4);
    assert!(
        value.contains("effects: !{io}"),
        "hover on println must render `effects: !{{io}}`; got {value:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_uuid_v4_shows_io_random_effects() {
    let mut client = LspClient::spawn();
    let uri = unique_uri();
    let src = "import uuid\nfn main() {\n  uuid.v4()\n}\n";
    client.did_open_and_wait(&uri, src);
    let value = hover_value(&mut client, &uri, 2, 8);
    assert!(
        value.contains("effects: !{io, random}"),
        "hover on uuid.v4 must render `effects: !{{io, random}}`; got {value:?}"
    );
    client.shutdown();
}
