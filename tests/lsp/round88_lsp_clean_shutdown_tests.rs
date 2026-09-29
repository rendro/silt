//! Round 88 — LSP graceful shutdown regression.
//!
//! Reproduces the latent bug where `silt lsp` hangs after a clean
//! `shutdown`/`exit` handshake. Root cause was that `Server::run`
//! returning did not drop the writer-side channel `Sender`
//! (`connection.sender` lives inside `Server`), so `io_threads.join()`
//! parked forever on the writer thread. The fix drops `server` before
//! calling `join`. This test catches the regression by spawning the
//! real `silt lsp` binary, driving the full init / shutdown / exit
//! sequence over stdio, and asserting the process exits cleanly within
//! 3 s — i.e. without SIGKILL.
//!
//! The pattern is intentionally minimal vs. other lsp_*_tests.rs files:
//! no diagnostics, no didOpen, no semantic-tokens — only the shutdown
//! handshake itself, so the test stays small and fast.
//!
//! NB: the test fails outright on timeout. It does NOT fall back to
//! `child.kill()` — doing so would mask the very hang we want to catch.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const READ_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

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
        let child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn silt lsp");
        let mut child = child;
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = channel::<Value>();
        thread::spawn(move || reader_loop(stdout, tx));
        LspClient { child, stdin, rx }
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
}

/// Drive the full init / shutdown / exit handshake and assert the
/// child exits on its own within 3 s. The pid is incorporated into a
/// unique workspace `rootUri` so parallel test runs do not collide on
/// the same path string.
#[test]
fn lsp_exits_cleanly_on_shutdown_exit() {
    let mut client = LspClient::spawn();

    // 1) initialize → initialized
    let pid = std::process::id();
    let workspace = std::env::temp_dir().join(format!("silt_round88_lsp_{pid}"));
    let root_uri = format!("file://{}", workspace.display());
    client.send_raw(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "capabilities": {},
            "rootUri": root_uri,
        }
    }));
    let _ = client.recv_response_for(1);
    client.send_raw(&json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    }));

    // 2) shutdown → wait for response → exit
    client.send_raw(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "shutdown"
    }));
    let _ = client.recv_response_for(2);
    client.send_raw(&json!({"jsonrpc": "2.0", "method": "exit"}));

    // 3) Poll for exit within 3 s. The test deliberately does NOT call
    // child.kill() on timeout — that would mask the very hang we want
    // to catch.
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    let status = loop {
        match client.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Best-effort: kill the leaked child so we don't
                    // leave zombies behind when the assertion below
                    // panics. The kill happens AFTER we've decided the
                    // test fails, so it does not mask the bug.
                    let _ = client.child.kill();
                    let _ = client.child.wait();
                    panic!(
                        "silt lsp did not exit within {:?} after shutdown/exit handshake — \
                         regression of the LSP shutdown hang (writer thread parked because \
                         `connection.sender` was not dropped before `io_threads.join()`)",
                        SHUTDOWN_TIMEOUT
                    );
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("child.try_wait failed: {e}"),
        }
    };

    // 4) Clean exit. `silt lsp` does not currently call process::exit
    // with a specific code on graceful shutdown — Rust's default
    // termination yields success. Accept either success or any
    // signal-less exit code: the load-bearing assertion above is the
    // "did it exit at all" check.
    assert!(
        status.code().is_some(),
        "child exited via signal, not graceful exit: {status:?}"
    );
}
