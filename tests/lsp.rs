//! End-to-end integration tests for the Silt LSP server.
//!
//! These tests spawn the compiled `silt lsp` binary as a subprocess and
//! communicate with it over stdin/stdout using the LSP JSON-RPC framing
//! (`Content-Length: N\r\n\r\n{json}`). They exercise the protocol surface
//! end-to-end rather than calling internal helpers directly.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
static URI_COUNTER: AtomicU64 = AtomicU64::new(1);

/// How long we are willing to wait for a single message from the server
/// before declaring the test failed. Should be generous enough for a
/// debug-build cold-start on slow CI but short enough that a broken
/// server fails the test promptly rather than hanging forever.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

// ── Message plumbing ───────────────────────────────────────────────

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_test_{n}.silt")
}

/// A single JSON-RPC message from the server. The integration tests don't
/// care about the distinction between Response / Notification at the transport
/// layer — we just inspect the decoded `serde_json::Value`.
type ServerMessage = Value;

/// A client wrapping a running `silt lsp` subprocess. Reads are decoupled
/// onto a background thread so we can apply deterministic per-read timeouts
/// via an mpsc channel.
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

    /// Send a raw JSON-RPC message with LSP framing.
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

    /// Receive messages until we get a response matching `id`. Any
    /// intervening notifications are discarded. Fails the test on timeout.
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
                    // Drop notifications / other responses.
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

    /// Perform the full LSP initialization handshake: the `initialize`
    /// request and the `initialized` notification. Returns
    /// `(request_id, raw response)` so callers can assert that the response
    /// echoes the exact id they sent.
    fn initialize(&mut self) -> (u64, ServerMessage) {
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
        let resp = self.recv_response_for(id);
        self.send_notification("initialized", json!({}));
        (id, resp)
    }

    /// Send `textDocument/didOpen` and block until the first
    /// `publishDiagnostics` notification for the same URI arrives.
    fn did_open_and_wait(&mut self, uri: &str, source: &str) -> ServerMessage {
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
        // Publish may arrive asynchronously; loop until we see one for this URI.
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

    /// Perform a graceful shutdown / exit and wait for the subprocess to stop.
    fn shutdown(mut self) {
        let id = next_id();
        self.send_request(id, "shutdown", json!(null));
        // We don't strictly need to wait for the shutdown response — the
        // server will also exit on `exit` notification — but doing so
        // keeps the conversation well-formed.
        let _ = self.rx.recv_timeout(READ_TIMEOUT);
        self.send_notification("exit", json!(null));
        // Give the child a brief chance to exit cleanly.
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
        // Best-effort cleanup if a test panics before calling shutdown.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Background reader that parses `Content-Length: N\r\n\r\n{body}` frames
/// off the child's stdout and forwards each decoded JSON value to `tx`.
fn reader_loop<R: Read + Send + 'static>(stdout: R, tx: Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        // Read headers until the blank line that terminates them.
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return, // EOF
                Ok(_) => {}
                Err(_) => return,
            }
            // Headers are terminated by `\r\n\r\n`; an empty or "\r\n" line
            // marks the end of the header block.
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
            // Malformed header block — bail out.
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

// ── Tests ──────────────────────────────────────────────────────────

// ── 1. initialize handshake returns expected capabilities ──────────

#[test]
fn test_initialize_returns_capabilities() {
    let mut client = LspClient::spawn();
    let (id, resp) = client.initialize();

    assert_eq!(
        resp.get("id").and_then(|v| v.as_u64()),
        Some(id),
        "initialize response must echo id={id}, got: {resp}"
    );
    assert!(
        resp.get("error").is_none(),
        "initialize returned an error: {resp}"
    );

    let caps = resp
        .pointer("/result/capabilities")
        .expect("result.capabilities must be present");

    // The Silt LSP advertises hover, definition, completion, and document
    // formatting per src/lsp.rs::run(). Assert each one is present and
    // truthy (lsp-types may serialize `true` or `{"workDoneProgress":...}`).
    assert!(
        caps.get("hoverProvider").is_some(),
        "expected hoverProvider capability, got: {caps}"
    );
    assert!(
        caps.get("definitionProvider").is_some(),
        "expected definitionProvider capability, got: {caps}"
    );
    assert!(
        caps.get("completionProvider").is_some(),
        "expected completionProvider capability, got: {caps}"
    );
    assert!(
        caps.get("documentFormattingProvider").is_some(),
        "expected documentFormattingProvider capability, got: {caps}"
    );
    // textDocumentSync is set to FULL sync in run().
    assert!(
        caps.get("textDocumentSync").is_some(),
        "expected textDocumentSync capability, got: {caps}"
    );

    client.shutdown();
}

// ── 2. didOpen with valid program produces no diagnostics ──────────

#[test]
fn test_did_open_valid_program_no_diagnostics() {
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    let uri = unique_uri();
    let source = "fn main() {\n  println(\"hello\")\n}\n";
    let publish = client.did_open_and_wait(&uri, source);

    let diags = publish
        .pointer("/params/diagnostics")
        .and_then(|v| v.as_array())
        .expect("diagnostics array must be present");
    assert!(
        diags.is_empty(),
        "expected no diagnostics for a valid program, got: {diags:?}"
    );

    client.shutdown();
}

// ── 3. didOpen with type error produces a diagnostic at the right
//      location ────────────────────────────────────────────────────

#[test]
fn test_did_open_type_error_produces_diagnostic() {
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    // `let x: Int = "hello"` is a clear type mismatch — the CLI tests in
    // tests/cli.rs rely on the same snippet producing a type error.
    let uri = unique_uri();
    let source = "fn main() {\n  let x: Int = \"hello\"\n}\n";
    let publish = client.did_open_and_wait(&uri, source);

    let diags = publish
        .pointer("/params/diagnostics")
        .and_then(|v| v.as_array())
        .expect("diagnostics array must be present");

    assert!(
        !diags.is_empty(),
        "expected at least one diagnostic for a type-error program"
    );

    // Find a diagnostic whose message mentions "type mismatch".
    let diag = diags
        .iter()
        .find(|d| {
            d.get("message")
                .and_then(|m| m.as_str())
                .map(|s| s.contains("type mismatch"))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("no 'type mismatch' diagnostic found, got: {diags:?}"));

    // Severity = ERROR = 1 in LSP.
    assert_eq!(
        diag.get("severity").and_then(|v| v.as_u64()),
        Some(1),
        "diagnostic should be Error severity, got: {diag}"
    );

    // The diagnostic must point somewhere on the second line (0-indexed: 1),
    // because that's where `let x: Int = "hello"` lives in our source.
    let line = diag
        .pointer("/range/start/line")
        .and_then(|v| v.as_u64())
        .expect("diagnostic must have a range.start.line");
    assert_eq!(
        line, 1,
        "type-error diagnostic should be on line index 1 (the `let` line), got line {line}"
    );

    // The diagnostic's range must not be a degenerate (0,0)-(0,0) span.
    let end_char = diag
        .pointer("/range/end/character")
        .and_then(|v| v.as_u64())
        .expect("diagnostic must have a range.end.character");
    assert!(
        end_char > 0,
        "diagnostic end character must be > 0, got: {diag}"
    );

    client.shutdown();
}

// ── 4. hover on an identifier returns an inferred type ─────────────

#[test]
fn test_hover_returns_inferred_type() {
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    // Hover on the `answer` *reference* (not the declaration) — the Silt LSP
    // reads types off the typed AST, which annotates expressions, so the
    // identifier must appear in an expression position (here: a call arg).
    //
    //   line 0: fn main() {
    //   line 1:   let answer = 42
    //   line 2:   println(answer)
    //   line 3: }
    let uri = unique_uri();
    let source = "fn main() {\n  let answer = 42\n  println(answer)\n}\n";
    let _ = client.did_open_and_wait(&uri, source);

    // Position: line 2, character 11 — somewhere inside `answer` in
    // "  println(answer)":
    //   0         1
    //   0123456789012345
    //              ^— 'a' of `answer` is at column 10, 'n' at 11.
    let id = next_id();
    client.send_request(
        id,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 11 }
        }),
    );
    let resp = client.recv_response_for(id);

    assert!(
        resp.get("error").is_none(),
        "hover request returned an error: {resp}"
    );

    let result = resp
        .get("result")
        .expect("hover response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null hover result on an identifier with a known type"
    );

    // The server returns Hover { contents: MarkupContent { kind: "markdown",
    // value: "```silt\n<Type>\n```" }, ... }. We just check that the value
    // contains `Int` — the type of the literal `42`.
    let value = result
        .pointer("/contents/value")
        .and_then(|v| v.as_str())
        .expect("hover result must contain contents.value string");
    assert!(
        value.contains("Int"),
        "expected hover to mention `Int`, got: {value}"
    );

    client.shutdown();
}

// ── 5. textDocument/definition on an identifier returns a location ─

#[test]
fn test_goto_definition_returns_location() {
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    // Two functions — `helper` defined at line 0, called from inside `main`
    // at line 3. Asking for the definition of `helper` on the call site
    // should return a Location pointing at the `fn helper` declaration.
    //
    //   line 0: fn helper() {
    //   line 1:   println("helped")
    //   line 2: }
    //   line 3: fn main() {
    //   line 4:   helper()
    //   line 5: }
    let uri = unique_uri();
    let source = "fn helper() {\n  println(\"helped\")\n}\nfn main() {\n  helper()\n}\n";
    let _ = client.did_open_and_wait(&uri, source);

    // Position of 'h' in `helper()` on line 4, columns "  helper()":
    //   0123456
    let id = next_id();
    client.send_request(
        id,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 4, "character": 3 }
        }),
    );
    let resp = client.recv_response_for(id);

    assert!(
        resp.get("error").is_none(),
        "definition request returned an error: {resp}"
    );

    let result = resp
        .get("result")
        .expect("definition response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null definition result for a known identifier"
    );

    // The server returns `GotoDefinitionResponse::Scalar(Location { uri, range })`
    // which serializes as a single Location object.
    let def_uri = result
        .get("uri")
        .and_then(|v| v.as_str())
        .expect("definition result must have a uri");
    assert_eq!(
        def_uri, uri,
        "definition must point back into the same document"
    );

    let line = result
        .pointer("/range/start/line")
        .and_then(|v| v.as_u64())
        .expect("definition result must have a range.start.line");
    assert_eq!(
        line, 0,
        "definition of `helper` should be on line index 0, got line {line}"
    );

    client.shutdown();
}

// ── 6. completion returns a list including keywords ────────────────

#[test]
fn test_completion_returns_keywords() {
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    let uri = unique_uri();
    let source = "fn main() {\n  \n}\n";
    let _ = client.did_open_and_wait(&uri, source);

    // Request completions from inside the function body (line 1, col 2).
    let id = next_id();
    client.send_request(
        id,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 2 }
        }),
    );
    let resp = client.recv_response_for(id);

    assert!(
        resp.get("error").is_none(),
        "completion request returned an error: {resp}"
    );

    let result = resp
        .get("result")
        .expect("completion response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null completion result at a valid position"
    );

    // Completion can serialize as either a plain array or `{items:[...]}`.
    let items: &Vec<Value> = if let Some(arr) = result.as_array() {
        arr
    } else if let Some(arr) = result.pointer("/items").and_then(|v| v.as_array()) {
        arr
    } else {
        panic!("unexpected completion result shape: {result}");
    };

    assert!(
        !items.is_empty(),
        "expected at least one completion item, got empty list"
    );

    // The server always emits keyword completions from the KEYWORDS table
    // (see src/lsp.rs completion handler). `fn` and `let` are core keywords
    // that should always be offered.
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|it| it.get("label").and_then(|v| v.as_str()))
        .collect();
    assert!(
        labels.contains(&"fn"),
        "expected `fn` keyword in completion list, got: {labels:?}"
    );
    assert!(
        labels.contains(&"let"),
        "expected `let` keyword in completion list, got: {labels:?}"
    );

    client.shutdown();
}

// ── 7. completion returns local bindings in scope ──────────────────

#[test]
fn test_completion_returns_local_bindings() {
    // A completion request inside the body of `main` — after a local
    // `greeting` has been bound — must include `greeting` as a candidate.
    // This guards against a regression where only keywords/builtins are
    // offered and user locals are dropped from the symbol/completion path.
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    let uri = unique_uri();
    // Line indices (0-based):
    //   0: fn main() {
    //   1:   let greeting = "hi"
    //   2:   gree
    //   3: }
    let source = "fn main() {\n  let greeting = \"hi\"\n  gree\n}\n";
    let _ = client.did_open_and_wait(&uri, source);

    // Request completions right after `gree` on line 2. Column index 6
    // is the end of "  gree" (two spaces + four characters).
    let id = next_id();
    client.send_request(
        id,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 6 }
        }),
    );
    let resp = client.recv_response_for(id);

    assert!(
        resp.get("error").is_none(),
        "completion request returned an error: {resp}"
    );

    let result = resp
        .get("result")
        .expect("completion response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null completion result at a valid position"
    );

    let items: &Vec<Value> = if let Some(arr) = result.as_array() {
        arr
    } else if let Some(arr) = result.pointer("/items").and_then(|v| v.as_array()) {
        arr
    } else {
        panic!("unexpected completion result shape: {result}");
    };

    let labels: Vec<&str> = items
        .iter()
        .filter_map(|it| it.get("label").and_then(|v| v.as_str()))
        .collect();

    assert!(
        labels.contains(&"greeting"),
        "expected local binding `greeting` in completion list, got: {labels:?}"
    );

    client.shutdown();
}

// ── 8. dot-completion surfaces stdlib module members ──────────────

#[test]
fn test_completion_returns_module_members_after_dot() {
    // After `string.` inside a function body, the completion list should
    // include at least one well-known function from the `string` stdlib
    // module such as `length` or `contains`.
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    let uri = unique_uri();
    // Line indices (0-based):
    //   0: import string
    //   1: fn main() {
    //   2:   string.
    //   3: }
    let source = "import string\nfn main() {\n  string.\n}\n";
    let _ = client.did_open_and_wait(&uri, source);

    // Cursor right after the `.` on line 2. "  string." is 9 columns.
    let id = next_id();
    client.send_request(
        id,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 9 }
        }),
    );
    let resp = client.recv_response_for(id);

    assert!(
        resp.get("error").is_none(),
        "completion request returned an error: {resp}"
    );

    let result = resp
        .get("result")
        .expect("completion response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null completion result after `string.`"
    );

    let items: &Vec<Value> = if let Some(arr) = result.as_array() {
        arr
    } else if let Some(arr) = result.pointer("/items").and_then(|v| v.as_array()) {
        arr
    } else {
        panic!("unexpected completion result shape: {result}");
    };

    let labels: Vec<&str> = items
        .iter()
        .filter_map(|it| it.get("label").and_then(|v| v.as_str()))
        .collect();

    // At least one well-known `string` module member must appear.
    let has_known_member = labels.iter().any(|l| *l == "length" || *l == "contains");
    assert!(
        has_known_member,
        "expected dot-completion after `string.` to include `length` or `contains`, got: {labels:?}"
    );

    client.shutdown();
}

// ── 9. hover on a let-binding site returns the binding's type (B9) ──

#[test]
fn test_hover_on_let_binding_site_returns_binding_type() {
    // Regression test for B9: hovering the identifier `x` *at its binding
    // site* (`let x = 42`) used to return the enclosing block's `()` Unit
    // type instead of `Int`. The fix makes the hover handler notice when
    // the cursor sits on a local binding LHS and return its definition
    // type instead.
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    let uri = unique_uri();
    //   line 0: fn main() {
    //   line 1:   let x = 42
    //   line 2: }
    let source = "fn main() {\n  let x = 42\n}\n";
    let _ = client.did_open_and_wait(&uri, source);

    // Position: line 1, character 6 — the `x` in "  let x = 42":
    //   0         1
    //   0123456789012
    //         ^— 'x' is at column 6.
    let id = next_id();
    client.send_request(
        id,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 6 }
        }),
    );
    let resp = client.recv_response_for(id);

    assert!(
        resp.get("error").is_none(),
        "hover request returned an error: {resp}"
    );

    let result = resp
        .get("result")
        .expect("hover response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null hover result on the binding `x`"
    );

    let value = result
        .pointer("/contents/value")
        .and_then(|v| v.as_str())
        .expect("hover result must contain contents.value string");

    // The binding's type is `Int` (from the 42 literal), not `()` / Unit.
    assert!(
        value.contains("Int"),
        "hover on `let x = 42` binding site should mention `Int`, got: {value}"
    );
    assert!(
        !value.contains("()"),
        "hover on binding site should not return the enclosing block's Unit type, got: {value}"
    );

    client.shutdown();
}

// ── 10. goto-definition on a local variable use site (G5) ──────────

#[test]
fn test_goto_definition_on_local_variable() {
    // Regression test for G5: `textDocument/definition` on a local binding
    // used to return `null` because the definitions map only contained
    // top-level declarations. The fix adds a per-document locals table so
    // the request resolves to the `x` in `let x = 42`.
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    let uri = unique_uri();
    //   line 0: fn main() {
    //   line 1:   let x = 42
    //   line 2:   println(x)
    //   line 3: }
    let source = "fn main() {\n  let x = 42\n  println(x)\n}\n";
    let _ = client.did_open_and_wait(&uri, source);

    // Position: line 2, character 10 — the `x` inside `println(x)`:
    //   0         1
    //   01234567890
    //             ^— `x` is at column 10.
    let id = next_id();
    client.send_request(
        id,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 10 }
        }),
    );
    let resp = client.recv_response_for(id);

    assert!(
        resp.get("error").is_none(),
        "definition request returned an error: {resp}"
    );

    let result = resp
        .get("result")
        .expect("definition response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null definition result for a local binding, got null"
    );

    let def_uri = result
        .get("uri")
        .and_then(|v| v.as_str())
        .expect("definition result must have a uri");
    assert_eq!(
        def_uri, uri,
        "definition must point back into the same document"
    );

    // The binding `x` is on line 1 (`  let x = 42`).
    let line = result
        .pointer("/range/start/line")
        .and_then(|v| v.as_u64())
        .expect("definition result must have a range.start.line");
    assert_eq!(
        line, 1,
        "definition of local `x` should be on line index 1 (the `let`), got line {line}"
    );

    // And the character should be 6 (the `x` in `  let x = 42`).
    let character = result
        .pointer("/range/start/character")
        .and_then(|v| v.as_u64())
        .expect("definition result must have a range.start.character");
    assert_eq!(
        character, 6,
        "definition of local `x` should be at column 6 (the `x` in `let x`), got character {character}"
    );

    client.shutdown();
}

// ── 11. textDocument/formatting returns edits for unformatted source ─
//
// Regression: guards `fn format()` in src/lsp.rs (the `Formatting::METHOD`
// dispatch). If the formatting handler ever stopped running the formatter,
// stopped computing the full-document replacement edit, or started
// returning `None`/`[]` for clearly-unformatted input, this test would
// fail. The snippet below is intentionally ugly ( `let x=42` has no spaces
// around `=`, and the body is flush-left inside the block) so the
// formatter definitely produces a different string, forcing at least one
// TextEdit in the response.

#[test]
fn test_formatting_returns_edits() {
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    let uri = unique_uri();
    // Deliberately unformatted: no spaces around `=`, body not indented.
    let source = "fn main() {\nlet x=42\nprintln(x)\n}\n";
    let _ = client.did_open_and_wait(&uri, source);

    let id = next_id();
    client.send_request(
        id,
        "textDocument/formatting",
        json!({
            "textDocument": { "uri": uri },
            "options": {
                "tabSize": 2,
                "insertSpaces": true,
            }
        }),
    );
    let resp = client.recv_response_for(id);

    assert!(
        resp.get("error").is_none(),
        "formatting request returned an error: {resp}"
    );

    let result = resp
        .get("result")
        .expect("formatting response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null formatting result for a known document"
    );

    let edits = result
        .as_array()
        .expect("formatting result must be an array of TextEdit");
    assert!(
        !edits.is_empty(),
        "expected at least one TextEdit when formatting clearly-unformatted source, got empty list"
    );

    // Each edit must have both a `range` and a `newText` field. The
    // implementation returns a single whole-document replacement, so check
    // that the first edit has a non-empty `newText` that differs from the
    // original source — this is the "actually formatted" signal.
    let edit = &edits[0];
    assert!(
        edit.get("range").is_some(),
        "TextEdit must have a range, got: {edit}"
    );
    let new_text = edit
        .get("newText")
        .and_then(|v| v.as_str())
        .expect("TextEdit must have a newText string");
    assert!(
        !new_text.is_empty(),
        "newText must not be empty for an unformatted document"
    );
    assert_ne!(
        new_text, source,
        "newText must differ from the original source for unformatted input"
    );
    // The formatter canonicalizes `let x=42` to `let x = 42`. If that
    // normalization ever stops happening, the edit's new text will not
    // contain the spaced form.
    assert!(
        new_text.contains("let x = 42"),
        "expected formatted output to contain `let x = 42`, got: {new_text}"
    );

    client.shutdown();
}

// ── 12. textDocument/signatureHelp returns a signature with parameters ─
//
// Regression: guards `fn signature_help()` in src/lsp.rs (the
// `SignatureHelpRequest::METHOD` dispatch). If the handler ever stopped
// locating the enclosing call, stopped looking up the definition, or
// stopped building a SignatureInformation label from `DefInfo`, this test
// would fail. Uses a user-defined `fn add(a: Int, b: Int) -> Int` so the
// assertion on the label is deterministic (not dependent on the stdlib).

#[test]
fn test_signature_help_returns_arity() {
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    let uri = unique_uri();
    //   line 0: fn add(a: Int, b: Int) -> Int {
    //   line 1:   a + b
    //   line 2: }
    //   line 3: fn main() {
    //   line 4:   add(
    //   line 5: }
    //
    // Cursor will sit just after the `(` of the `add(` call on line 4.
    let source =
        "fn add(a: Int, b: Int) -> Int {\n  a + b\n}\nfn main() {\n  add(\n}\n";
    let _ = client.did_open_and_wait(&uri, source);

    // "  add(" is 6 columns, cursor goes right after the `(`.
    let id = next_id();
    client.send_request(
        id,
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 4, "character": 6 }
        }),
    );
    let resp = client.recv_response_for(id);

    assert!(
        resp.get("error").is_none(),
        "signatureHelp request returned an error: {resp}"
    );

    let result = resp
        .get("result")
        .expect("signatureHelp response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null signatureHelp result inside a known call"
    );

    let sigs = result
        .pointer("/signatures")
        .and_then(|v| v.as_array())
        .expect("signatureHelp result must have a `signatures` array");
    assert!(
        !sigs.is_empty(),
        "signatures array must contain at least one SignatureInformation"
    );

    let sig = &sigs[0];
    let label = sig
        .get("label")
        .and_then(|v| v.as_str())
        .expect("SignatureInformation must have a label string");

    // `build_signature_from_def` renders a typed Fun as
    //   fn add(a: Int, b: Int) -> Int
    // — assert on both parameter names and the return type.
    assert!(
        label.contains("add"),
        "signature label must mention the function name `add`, got: {label}"
    );
    assert!(
        label.contains("a: Int"),
        "signature label must mention the first parameter `a: Int`, got: {label}"
    );
    assert!(
        label.contains("b: Int"),
        "signature label must mention the second parameter `b: Int`, got: {label}"
    );
    assert!(
        label.contains("-> Int"),
        "signature label must mention the return type `-> Int`, got: {label}"
    );

    // The `parameters` field must reflect the function's arity (2).
    let params = sig
        .get("parameters")
        .and_then(|v| v.as_array())
        .expect("SignatureInformation must have a parameters array");
    assert_eq!(
        params.len(),
        2,
        "signature must have 2 parameters for `fn add(a, b)`, got: {params:?}"
    );

    // At the cursor (just after `(`, before any arg), active_parameter is 0.
    let active = sig
        .get("activeParameter")
        .and_then(|v| v.as_u64())
        .expect("SignatureInformation must have activeParameter");
    assert_eq!(
        active, 0,
        "activeParameter must be 0 before any comma has been typed, got: {active}"
    );

    client.shutdown();
}

// ── 13. textDocument/documentSymbol lists top-level declarations ───
//
// Regression: guards `fn document_symbols()` in src/lsp.rs (the
// `DocumentSymbolRequest::METHOD` dispatch). If the handler ever stopped
// walking the program's declarations, stopped including Fn/Type decls, or
// regressed on the serialization shape, this test would fail. The snippet
// has a function, a record type, and a top-level `let` so all three
// branches of the handler are exercised at least shallowly.

#[test]
fn test_document_symbols_lists_top_level_defs() {
    let mut client = LspClient::spawn();
    let _ = client.initialize();

    let uri = unique_uri();
    //   line 0: type Point {
    //   line 1:   x: Int,
    //   line 2:   y: Int,
    //   line 3: }
    //   line 4: fn origin() -> Point {
    //   line 5:   Point { x: 0, y: 0 }
    //   line 6: }
    //   line 7: fn main() {
    //   line 8:   let _p = origin()
    //   line 9: }
    let source = "type Point {\n  x: Int,\n  y: Int,\n}\n\
                  fn origin() -> Point {\n  Point { x: 0, y: 0 }\n}\n\
                  fn main() {\n  let _p = origin()\n}\n";
    let _ = client.did_open_and_wait(&uri, source);

    let id = next_id();
    client.send_request(
        id,
        "textDocument/documentSymbol",
        json!({
            "textDocument": { "uri": uri }
        }),
    );
    let resp = client.recv_response_for(id);

    assert!(
        resp.get("error").is_none(),
        "documentSymbol request returned an error: {resp}"
    );

    let result = resp
        .get("result")
        .expect("documentSymbol response must have a `result` field");
    assert!(
        !result.is_null(),
        "expected non-null documentSymbol result for a document with declarations"
    );

    // The server returns `DocumentSymbolResponse::Nested(...)`, which serializes
    // as a bare array of DocumentSymbol. Fall back to `{items: [...]}` just in
    // case the serialization shape ever varies.
    let symbols: &Vec<Value> = if let Some(arr) = result.as_array() {
        arr
    } else {
        panic!("unexpected documentSymbol result shape: {result}");
    };

    assert!(
        !symbols.is_empty(),
        "expected at least one top-level DocumentSymbol, got empty list"
    );

    // Collect (name, kind) tuples. LSP SymbolKind values: FUNCTION=12,
    // STRUCT=23, VARIABLE=13 — but we assert on names + the presence of the
    // kind field rather than pinning to the exact numeric values, which are
    // implementation details of the lsp-types crate.
    let names: Vec<&str> = symbols
        .iter()
        .filter_map(|s| s.get("name").and_then(|v| v.as_str()))
        .collect();

    assert!(
        names.contains(&"origin"),
        "expected top-level function `origin` in document symbols, got: {names:?}"
    );
    assert!(
        names.contains(&"main"),
        "expected top-level function `main` in document symbols, got: {names:?}"
    );
    assert!(
        names.contains(&"Point"),
        "expected top-level type `Point` in document symbols, got: {names:?}"
    );

    // Every symbol must carry a `kind` and a `range` — these are required
    // by the LSP spec and by the editor UIs that consume them.
    for sym in symbols {
        assert!(
            sym.get("kind").is_some(),
            "DocumentSymbol must have a `kind`, got: {sym}"
        );
        assert!(
            sym.get("range").is_some(),
            "DocumentSymbol must have a `range`, got: {sym}"
        );
        assert!(
            sym.get("selectionRange").is_some(),
            "DocumentSymbol must have a `selectionRange`, got: {sym}"
        );
    }

    // Locate the `origin` symbol specifically and assert it's reported as
    // a function kind (LSP SymbolKind::FUNCTION == 12). This guards against
    // a regression where Fn decls get mislabeled as Variable etc.
    let origin_sym = symbols
        .iter()
        .find(|s| s.get("name").and_then(|v| v.as_str()) == Some("origin"))
        .expect("origin symbol must be present");
    assert_eq!(
        origin_sym.get("kind").and_then(|v| v.as_u64()),
        Some(12),
        "`origin` must be reported as SymbolKind::FUNCTION (12), got: {origin_sym}"
    );

    client.shutdown();
}
