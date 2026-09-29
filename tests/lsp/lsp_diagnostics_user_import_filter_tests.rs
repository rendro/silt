//! LSP regression tests for GAP #8: `import <user_module>` must NOT
//! produce the "unknown module" warning (nor follow-on "undefined X"
//! errors for names imported through it) in the editor. The LSP used to
//! publish every typechecker diagnostic unfiltered; since the checker
//! has no filesystem access, every legitimate user-module import was
//! flagged. These tests lock the fix in `src/lsp/diagnostics.rs`
//! `update_document` and keep future refactors honest.
//!
//! Communicates with the compiled `silt lsp` subprocess end-to-end so
//! we exercise the real pipeline.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
static URI_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generous enough for a debug-build cold start on slow CI but short
/// enough that a broken server fails the test promptly.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_user_import_filter_{n}.silt")
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
        // Drain the initialize response.
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::from_millis(0));
            if remaining.is_zero() {
                panic!("timed out waiting for initialize response");
            }
            match self.rx.recv_timeout(remaining) {
                Ok(msg) => {
                    if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("timed out waiting for initialize response");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("silt lsp server closed its stdout unexpectedly");
                }
            }
        }
        self.send_notification("initialized", json!({}));
    }

    /// `didOpen` + block on the first `publishDiagnostics` for this URI.
    fn did_open_and_wait(&mut self, uri: &str, source: &str) -> Value {
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
                        return msg;
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

fn diagnostic_messages(notif: &Value) -> Vec<String> {
    notif
        .pointer("/params/diagnostics")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|d| {
                    d.get("message")
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── Tests ──────────────────────────────────────────────────────────

/// A silt file that imports a user module and uses a name from it must
/// not surface the typechecker's "unknown module" warning or the
/// follow-on "undefined variable" error through LSP diagnostics.
#[test]
fn test_lsp_filters_unknown_module_warning_for_user_import() {
    let mut client = LspClient::spawn();
    client.initialize();

    let uri = unique_uri();
    let source = "import my_user_module\nfn main() { println(my_user_module.something()) }\n";
    let notif = client.did_open_and_wait(&uri, source);
    let messages = diagnostic_messages(&notif);

    for m in &messages {
        assert!(
            !m.contains("unknown module"),
            "LSP must filter the 'unknown module' warning for user imports; got: {m:?}"
        );
        assert!(
            !m.starts_with("undefined variable"),
            "LSP must filter 'undefined variable' follow-ons for user imports; got: {m:?}"
        );
    }

    client.shutdown();
}

/// The filter must not hide real errors that happen alongside a
/// user-module import. Here we intentionally call `add` with the wrong
/// arity — an error the typechecker reports as
/// `` `add` expects 2 arguments, got 1 ``, which is outside the
/// user-import filter's pattern list. The filter must NOT swallow it.
#[test]
fn test_lsp_still_reports_real_type_errors_with_user_import() {
    let mut client = LspClient::spawn();
    client.initialize();

    let uri = unique_uri();
    // Contains:
    //   (a) a user-module import that triggers the filtered warning
    //   (b) an arity error on a local fn the typechecker catches; the
    //       arity-mismatch message doesn't match any user-import filter
    //       pattern, so it must still surface.
    let source = "import my_user_module\nfn add(a, b) { a + b }\nfn main() { add(1) }\n";
    let notif = client.did_open_and_wait(&uri, source);
    let messages = diagnostic_messages(&notif);

    // No filtered noise.
    for m in &messages {
        assert!(
            !m.contains("unknown module"),
            "LSP must filter 'unknown module'; got: {m:?}"
        );
    }

    // The real arity error must still be present.
    let has_arity_error = messages
        .iter()
        .any(|m| m.contains("expects 2 arguments, got 1"));
    assert!(
        has_arity_error,
        "LSP must still surface real type errors alongside a user import; got diagnostics: {messages:?}"
    );

    client.shutdown();
}
