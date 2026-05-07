//! Round-76 D3: hover on a stdlib module-method reference must NOT
//! render the top-of-hover signature with an unresolved TyVar
//! (`Fn(String) -> _`). The markdown body below the `---` is
//! authoritative; the redundant top signature must agree (or, where
//! that's hard, be suppressed when it would carry unresolved vars).
//!
//! Lock: real LSP `textDocument/hover` request, asserting the response
//! does NOT contain `-> _` for `string.length`. Test must FAIL before
//! the fix and PASS after.

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
                Err(RecvTimeoutError::Timeout) => panic!("timeout id={id}"),
                Err(RecvTimeoutError::Disconnected) => panic!("disconnected id={id}"),
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
                panic!("diagnostic timeout for {uri}");
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
                Err(_) => panic!("diagnostic recv error"),
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

/// Hover on `length` in `string.length(s)` when the user FORGOT
/// `import string`. The bug:
///
///     ```silt
///     Fn(String) -> _
///     ```
///     effects: !{}
///     ---
///     string.length(s: String) -> Int
///     ...
///
/// The top fenced block contradicted the markdown body: an unresolved
/// `Type::Var` in the return slot rendered as `_`. Cause: the
/// FieldAccess arm of inference (src/typechecker/inference.rs:2329)
/// stores a fresh `Type::Var` for unimported-builtin-module references
/// instead of the scheme's actual type, then the Call arm partially
/// unifies it (param side only) so post-resolve `expr.ty` becomes
/// `Fn(String, fresh_ret)` with `fresh_ret` never unified. The fix
/// suppresses the top signature block whenever the rendered type has
/// unresolved TyVars AND a markdown signature is available below.
#[test]
fn hover_on_string_length_does_not_render_unresolved_return_type() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r76_hover_string_length.silt";
    // No `import string` — the FieldAccess arm of inference will record
    // a fresh TyVar for the qualified reference; the Call arm later
    // partially unifies it so post-resolve we get `Fn(String, Var(N))`.
    let src = "fn main() {\n  let s = \"hi\"\n  string.length(s)\n}\n";
    client.did_open_and_wait(file, src);

    // line 2 = `  string.length(s)` ; cursor at character 12 lands
    // inside `length`.
    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 2, "character": 12 }
        }),
    );

    let value = resp
        .pointer("/result/contents/value")
        .and_then(|v| v.as_str())
        .expect("hover result has markdown value");

    // The bug: hover renders `Fn(String) -> _` at the top. Lock that we
    // never produce that text. Also lock that no `-> _` appears in any
    // form (covers `Fn(_, _) -> _` shapes for sibling cases).
    assert!(
        !value.contains("-> _"),
        "hover for `string.length` must not render `-> _` (unresolved \
         TyVar in return position); got:\n{value}"
    );
    // Affirmative: we still surface the markdown signature with the
    // resolved return type `Int`.
    assert!(
        value.contains("string.length(s: String) -> Int"),
        "hover for `string.length` should surface the markdown signature; got:\n{value}"
    );

    client.shutdown();
}

/// Affirmative companion: when `import string` IS present and the call
/// fully unifies, the top signature block IS rendered (with the
/// resolved return type). Locks that the D3 fix is narrowly scoped to
/// the unresolved-var case and does not regress the imported path.
#[test]
fn hover_imported_string_length_renders_top_signature_with_int() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r76_hover_imported_ok.silt";
    let src = "import string\nfn main() {\n  let s = \"hi\"\n  string.length(s)\n}\n";
    client.did_open_and_wait(file, src);

    // line 3 col 12 = inside `length`.
    let resp = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": file },
            "position": { "line": 3, "character": 12 }
        }),
    );
    let value = resp
        .pointer("/result/contents/value")
        .and_then(|v| v.as_str())
        .expect("hover result has markdown value");

    // Top signature renders with resolved Int return.
    assert!(
        value.contains("Fn(String) -> Int"),
        "imported `string.length` hover should render top signature with \
         resolved Int return; got:\n{value}"
    );
    // And the markdown signature is also present.
    assert!(
        value.contains("string.length(s: String) -> Int"),
        "markdown signature should be present; got:\n{value}"
    );

    client.shutdown();
}
