//! End-to-end LSP test for `textDocument/semanticTokens/full`.
//!
//! Mirrors the subprocess harness in `tests/lsp_workspace_tests.rs`:
//! spawns `silt lsp` and speaks LSP JSON-RPC over stdio.

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

/// Indexes into `TOKEN_LEGEND` — must match
/// `src/lsp/semantic_tokens.rs::TOKEN_LEGEND`.
const TT_FUNCTION: u64 = 0;
const TT_TYPE: u64 = 1;
const TT_ENUM: u64 = 2;
const TT_INTERFACE: u64 = 4;

#[test]
fn semantic_tokens_full_returns_classified_tokens() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_sem_tokens_a.silt";
    // Has: fn decl (foo), let-binding (x), type decl (Color), variant (Red),
    // trait decl (Show), method inside trait (show).
    let src = "fn foo() { let x = 42 }\ntype Color { Red }\ntrait Show { fn show(self) -> String }\n";
    client.did_open_and_wait(uri, src);

    let resp = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );

    let data = resp
        .pointer("/result/data")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected /result/data array; got: {resp}"));
    assert!(
        !data.is_empty(),
        "expected non-empty semantic tokens data; got: {resp}"
    );
    assert_eq!(
        data.len() % 5,
        0,
        "semantic tokens data length must be divisible by 5; got {}",
        data.len()
    );

    // Decode tokens (delta-encoded). Each is [deltaLine, deltaStart, length,
    // tokenType, tokenModifiers]. Reconstruct absolute positions so we can
    // verify the encoding is syntactically valid, and collect token types.
    let mut abs_line = 0i64;
    let mut abs_start = 0i64;
    let mut types: Vec<u64> = Vec::new();
    for chunk in data.chunks_exact(5) {
        let dl = chunk[0].as_i64().expect("deltaLine u32");
        let ds = chunk[1].as_i64().expect("deltaStart u32");
        let len = chunk[2].as_i64().expect("length u32");
        let tt = chunk[3].as_u64().expect("tokenType u32");
        let _mods = chunk[4].as_i64().expect("tokenModifiers u32");
        assert!(dl >= 0, "deltaLine must be non-negative");
        assert!(ds >= 0, "deltaStart must be non-negative");
        assert!(len > 0, "token length must be positive");
        if dl == 0 {
            abs_start += ds;
        } else {
            abs_line += dl;
            abs_start = ds;
        }
        assert!(abs_line >= 0 && abs_start >= 0);
        types.push(tt);
    }

    assert!(
        types.contains(&TT_FUNCTION),
        "expected a FUNCTION token for `foo`; got types: {types:?}"
    );
    assert!(
        types.contains(&TT_ENUM) || types.contains(&TT_TYPE),
        "expected a TYPE/ENUM token for `Color`; got types: {types:?}"
    );
    assert!(
        types.contains(&TT_INTERFACE),
        "expected an INTERFACE token for `Show`; got types: {types:?}"
    );

    client.shutdown();
}
