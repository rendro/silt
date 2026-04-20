//! Regression tests for Finding F8:
//!
//! Hover and goto-definition on identifiers introduced by a destructuring
//! `let` pattern used to return `null` because
//! `src/lsp/local_bindings.rs` (for block-scope `Stmt::Let`) and
//! `src/lsp/definitions.rs` (for top-level `Decl::Let`) only extracted
//! `PatternKind::Ident`. Tuple, record, and constructor patterns were
//! skipped entirely, so `let (a, b) = (1, 2)` left `a` and `b` invisible
//! to the LSP.
//!
//! Each test drives the real parse + typecheck + LSP hover/goto pipeline
//! via the stdio transport the way a client (VS Code, etc.) would, so it
//! locks down the end-to-end behaviour on both the local-bindings path
//! and the top-level `definitions` path.

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
    format!("file:///tmp/silt_lsp_destructured_{n}.silt")
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

// ── Helpers ────────────────────────────────────────────────────────

fn hover_value_at(client: &mut LspClient, uri: &str, line: u32, character: u32) -> Option<String> {
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
    let result = resp.get("result")?;
    if result.is_null() {
        return None;
    }
    Some(
        result
            .pointer("/contents/value")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("hover result missing contents.value: {result}"))
            .to_string(),
    )
}

fn goto_def_at(
    client: &mut LspClient,
    uri: &str,
    line: u32,
    character: u32,
) -> Option<(String, u64, u64)> {
    let id = next_id();
    client.send_request(
        id,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }),
    );
    let resp = client.recv_response_for(id);
    assert!(
        resp.get("error").is_none(),
        "definition request returned an error: {resp}"
    );
    let result = resp.get("result")?;
    if result.is_null() {
        return None;
    }
    let def_uri = result
        .get("uri")
        .and_then(|v| v.as_str())?
        .to_string();
    let line = result.pointer("/range/start/line").and_then(|v| v.as_u64())?;
    let character = result
        .pointer("/range/start/character")
        .and_then(|v| v.as_u64())?;
    Some((def_uri, line, character))
}

// ── Tests ──────────────────────────────────────────────────────────

// ── 1. Tuple destructure: hover + goto on usage of `a` ─────────────

#[test]
fn test_hover_on_tuple_destructure_usage() {
    // GAP (F8): `let (a, b) = (1, 2)` used to leave `a` and `b` invisible
    // to the LSP — hover on their usage returned null because the
    // binding was never registered as a local.  After the fix, hover
    // on the usage of `a` in `println(a + b)` must resolve to `Int`.
    //
    //   line 0: fn main() {
    //   line 1:   let (a, b) = (1, 2)
    //   line 2:   println(a + b)
    //   line 3: }
    let source = "fn main() {\n  let (a, b) = (1, 2)\n  println(a + b)\n}\n";

    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    // Line 2, column 10 is the `a` of `println(a + b)`:
    //   "  println(a + b)"
    //    0         1
    //    0123456789012
    let value = hover_value_at(&mut client, &uri, 2, 10)
        .expect("hover on usage of `a` must return a non-null result");
    assert!(
        value.contains("Int"),
        "hover on usage of `a` must resolve to `Int`, got: {value}"
    );

    client.shutdown();
}

#[test]
fn test_goto_def_on_tuple_destructure_usage() {
    // GAP (F8): goto-definition on a tuple-destructured binding used to
    // return `null`.  After the fix, goto-def on the usage of `a` must
    // point at the `a` in the pattern `(a, b)` on line 1 — column 7.
    //
    //   line 1: "  let (a, b) = (1, 2)"
    //            0         1
    //            0123456789012
    //                  ^ col 7 = `a`
    let source = "fn main() {\n  let (a, b) = (1, 2)\n  println(a + b)\n}\n";

    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    // Goto-def on the `a` of `println(a + b)` on line 2, col 10.
    let (def_uri, line, character) = goto_def_at(&mut client, &uri, 2, 10)
        .expect("goto-def on usage of `a` must return a non-null result");
    assert_eq!(def_uri, uri, "definition must point back into this document");
    assert_eq!(
        line, 1,
        "definition of `a` should be on line 1 (the `let` pattern), got {line}"
    );
    assert_eq!(
        character, 7,
        "definition of `a` should be at column 7 (the `a` in `(a, b)`), got {character}"
    );

    client.shutdown();
}

// ── 2. Nested tuple destructure: hover on each leaf ────────────────

#[test]
fn test_hover_on_nested_tuple_destructure_usage() {
    // GAP (F8): a nested destructure `let ((a, b), c) = ((1, 2), 3)`
    // must register `a`, `b`, and `c` as bindings with their resolved
    // element types.  Hover on each usage must return `Int`.
    //
    //   line 0: fn main() {
    //   line 1:   let ((a, b), c) = ((1, 2), 3)
    //   line 2:   println(a + b + c)
    //   line 3: }
    let source =
        "fn main() {\n  let ((a, b), c) = ((1, 2), 3)\n  println(a + b + c)\n}\n";

    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    // Line 2: "  println(a + b + c)"
    //          0         1
    //          0123456789012345678
    // col 10 = a, col 14 = b, col 18 = c
    for (col, name) in [(10u32, "a"), (14u32, "b"), (18u32, "c")] {
        let value = hover_value_at(&mut client, &uri, 2, col).unwrap_or_else(|| {
            panic!("hover on usage of `{name}` must return a non-null result")
        });
        assert!(
            value.contains("Int"),
            "hover on usage of `{name}` must resolve to `Int`, got: {value}"
        );
    }

    client.shutdown();
}

// ── 3. Record destructure: hover on usage ──────────────────────────

#[test]
fn test_hover_on_record_destructure_usage() {
    // GAP (F8): `let P { x, y } = P { x: 1, y: 2 }` must register `x`
    // and `y` as bindings whose types are propagated from the record's
    // declared field types. Hover on their usage must resolve to `Int`.
    //
    //   line 0: type P { x: Int, y: Int }
    //   line 1: fn main() {
    //   line 2:   let P { x, y } = P { x: 1, y: 2 }
    //   line 3:   println(x + y)
    //   line 4: }
    let source = "type P { x: Int, y: Int }\n\
                  fn main() {\n  \
                  let P { x, y } = P { x: 1, y: 2 }\n  \
                  println(x + y)\n\
                  }\n";

    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    // Line 3: "  println(x + y)"
    //          0         1
    //          01234567890123
    // col 10 = x, col 14 = y
    for (col, name) in [(10u32, "x"), (14u32, "y")] {
        let value = hover_value_at(&mut client, &uri, 3, col).unwrap_or_else(|| {
            panic!("hover on usage of `{name}` must return a non-null result")
        });
        assert!(
            value.contains("Int"),
            "hover on usage of `{name}` must resolve to `Int`, got: {value}"
        );
    }

    client.shutdown();
}

// ── 4. Top-level destructure: goto-def on usage ────────────────────

#[test]
fn test_goto_def_on_top_level_tuple_destructure_usage() {
    // GAP (F8): `let (a, b) = (1, 2)` at module level — the
    // `definitions.rs` path — used to skip destructuring patterns.
    // After the fix, goto-def on a usage of `a` inside `main` must
    // point back at the `a` on line 0 (column 5 in `let (a, b) = ...`).
    //
    //   line 0: let (a, b) = (1, 2)
    //           0         1
    //           0123456789
    //               ^ col 5 = `a`
    //   line 1: fn main() { println(a + b) }
    let source = "let (a, b) = (1, 2)\nfn main() { println(a + b) }\n";

    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    // Line 1: "fn main() { println(a + b) }"
    //          0         1         2
    //          0123456789012345678901234567
    // The `a` of `println(a + b)` is at column 20.
    let (def_uri, line, character) = goto_def_at(&mut client, &uri, 1, 20)
        .expect("goto-def on top-level destructured `a` must return a non-null result");
    assert_eq!(def_uri, uri, "definition must point back into this document");
    assert_eq!(
        line, 0,
        "definition of top-level `a` should be on line 0, got {line}"
    );
    assert_eq!(
        character, 5,
        "definition of top-level `a` should be at column 5 (the `a` in `(a, b)`), got {character}"
    );

    client.shutdown();
}
