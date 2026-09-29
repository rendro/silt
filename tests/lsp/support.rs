//! The one LSP test client shared by every module in this suite.
//!
//! `LspClient` spawns the compiled `silt lsp` binary and speaks LSP
//! JSON-RPC (`Content-Length: N\r\n\r\n{json}`) over its stdin/stdout.
//! Server messages are decoded on a background thread and handed over an
//! mpsc channel, so every read has a deterministic timeout. The child's
//! stderr is drained on a second thread (a full pipe would otherwise block
//! the server) and its tail is included in timeout panics.
//!
//! Every test spawns its own server, so request ids and URIs only need to
//! be unique within one test.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long we wait for a single message from the server before failing
/// the test: generous enough for a debug-build cold start on a loaded CI
/// machine, short enough that a broken server fails instead of hanging.
pub const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// How long `shutdown` gives the server to exit after `exit` before it is
/// killed.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

pub struct LspClient {
    pub child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl LspClient {
    /// Spawn `silt lsp` and complete the `initialize` / `initialized`
    /// handshake.
    pub fn spawn() -> Self {
        Self::spawn_with_root(None)
    }

    /// Like `spawn`, but sends `rootUri` in `initialize` when given.
    pub fn spawn_with_root(root_uri: Option<&str>) -> Self {
        let mut client = Self::spawn_uninitialized();
        client.initialize_with_root(root_uri);
        client
    }

    /// Spawn `silt lsp` without any handshake, for tests that drive
    /// `initialize` themselves.
    pub fn spawn_uninitialized() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn silt lsp");
        let stdin = child.stdin.take().expect("no stdin on child");
        let stdout = child.stdout.take().expect("no stdout on child");
        let mut stderr_pipe = child.stderr.take().expect("no stderr on child");

        let (tx, rx) = channel::<Value>();
        thread::spawn(move || reader_loop(stdout, tx));

        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&stderr);
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match stderr_pipe.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => sink.lock().unwrap().extend_from_slice(&buf[..n]),
                }
            }
        });

        LspClient {
            child,
            stdin,
            rx,
            stderr,
        }
    }

    /// Everything the server has written to stderr so far.
    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned()
    }

    fn fail(&self, what: &str) -> ! {
        let err = self.stderr.lock().map(|b| b.clone()).unwrap_or_default();
        let tail = String::from_utf8_lossy(&err[err.len().saturating_sub(4000)..]);
        panic!("{what}\n--- silt lsp stderr (tail) ---\n{tail}");
    }

    /// Send a raw JSON-RPC message with LSP framing.
    pub fn send_raw(&mut self, msg: &Value) {
        let body = serde_json::to_string(msg).expect("serialize");
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        self.stdin
            .write_all(framed.as_bytes())
            .expect("write to child stdin");
        self.stdin.flush().expect("flush child stdin");
    }

    pub fn send_request(&mut self, id: u64, method: &str, params: Value) {
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
    }

    pub fn send_notification(&mut self, method: &str, params: Value) {
        self.send_raw(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    /// Receive messages until `pred` accepts one and return it. Messages
    /// it rejects are dropped. Fails the test on timeout or disconnect.
    pub fn recv_until(&self, what: &str, mut pred: impl FnMut(&Value) -> bool) -> Value {
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.fail(&format!("timed out waiting for {what}"));
            }
            match self.rx.recv_timeout(remaining) {
                Ok(msg) if pred(&msg) => return msg,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    self.fail(&format!("timed out waiting for {what}"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.fail(&format!(
                        "silt lsp closed its stdout while waiting for {what}"
                    ));
                }
            }
        }
    }

    /// Receive messages until the response to request `id` arrives; any
    /// notifications and other responses in between are dropped.
    pub fn recv_response_for(&self, id: u64) -> Value {
        self.recv_until(&format!("response id={id}"), |msg| {
            msg.get("id").and_then(|v| v.as_u64()) == Some(id)
        })
    }

    /// The `initialize` request plus the `initialized` notification.
    /// Returns the request id and the raw response.
    pub fn initialize(&mut self) -> (u64, Value) {
        self.initialize_with_root(None)
    }

    pub fn initialize_with_root(&mut self, root_uri: Option<&str>) -> (u64, Value) {
        let id = next_id();
        let mut params = json!({ "capabilities": {} });
        if let Some(root) = root_uri {
            params["rootUri"] = json!(root);
        }
        self.send_request(id, "initialize", params);
        let resp = self.recv_response_for(id);
        self.send_notification("initialized", json!({}));
        (id, resp)
    }

    /// Send a request and return the whole response message.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = next_id();
        self.send_request(id, method, params);
        self.recv_response_for(id)
    }

    /// Send a request and return only its `result` (`null` if absent).
    pub fn request_result(&mut self, method: &str, params: Value) -> Value {
        let resp = self.request(method, params);
        resp.get("result").cloned().unwrap_or(Value::Null)
    }

    /// `textDocument/hover` at a position; returns the whole response.
    pub fn hover(&mut self, uri: &str, line: u32, character: u32) -> Value {
        self.request(
            "textDocument/hover",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            }),
        )
    }

    /// `textDocument/completion` at a position; returns the whole response.
    pub fn completion(&mut self, uri: &str, line: u32, character: u32) -> Value {
        self.request(
            "textDocument/completion",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            }),
        )
    }

    /// Send `textDocument/didOpen` (version 1) without waiting.
    pub fn did_open(&mut self, uri: &str, text: &str) {
        self.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "silt",
                    "version": 1,
                    "text": text,
                }
            }),
        );
    }

    /// Wait for the next `publishDiagnostics` notification for `uri`.
    pub fn wait_for_diagnostics(&self, uri: &str) -> Value {
        self.recv_until(&format!("publishDiagnostics for {uri}"), |msg| {
            msg.get("id").is_none()
                && msg.get("method").and_then(|v| v.as_str())
                    == Some("textDocument/publishDiagnostics")
                && msg.pointer("/params/uri").and_then(|v| v.as_str()) == Some(uri)
        })
    }

    /// Open a document and block until its first `publishDiagnostics`
    /// arrives, so later requests see the parsed document. Returns that
    /// notification.
    pub fn did_open_and_wait(&mut self, uri: &str, text: &str) -> Value {
        self.did_open(uri, text);
        self.wait_for_diagnostics(uri)
    }

    /// `did_open_and_wait`, returning just the `diagnostics` array.
    pub fn did_open_and_collect_diagnostics(&mut self, uri: &str, text: &str) -> Vec<Value> {
        self.did_open_and_wait(uri, text)
            .pointer("/params/diagnostics")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    /// `shutdown` / `exit`, then wait for the process to stop; it is
    /// killed if it has not exited within `EXIT_TIMEOUT`. Tests that
    /// assert a clean exit drive the handshake themselves.
    pub fn shutdown(mut self) {
        let id = next_id();
        self.send_raw(&json!({"jsonrpc": "2.0", "id": id, "method": "shutdown"}));
        // Best effort: a missing reply is not what these tests check.
        let deadline = Instant::now() + READ_TIMEOUT;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.rx.recv_timeout(remaining) {
                Ok(msg) if msg.get("id").and_then(|v| v.as_u64()) == Some(id) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        self.send_raw(&json!({"jsonrpc": "2.0", "method": "exit"}));
        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                // Timed out or `try_wait` failed: Drop kills the child.
                _ => return,
            }
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Reaps a child that already exited; kills one left running when a
        // test panics or `shutdown` timed out.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Parse `Content-Length: N\r\n\r\n{body}` frames off the child's stdout
/// and forward each decoded JSON value to `tx`. `BufReader` plus
/// `read_exact` reassembles frames split across pipe reads. Ends at EOF,
/// on a malformed frame, or when the client is gone.
fn reader_loop<R: Read + Send + 'static>(stdout: R, tx: Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        // Header lines up to the blank line that ends the block.
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.trim().eq_ignore_ascii_case("content-length")
                && let Ok(n) = value.trim().parse::<usize>()
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
