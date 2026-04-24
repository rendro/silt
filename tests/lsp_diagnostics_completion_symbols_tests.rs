//! Regression tests for three LSP GAPs:
//!
//! - **GAP A** (`src/lsp/diagnostics.rs`): the LSP must filter the
//!   typechecker's "unknown module" warning (and cascade "undefined"
//!   errors) for user-module imports so the editor Problems panel does
//!   not surface noise the CLI already filters out. Mirrors (copy-paste)
//!   the predicate in `src/cli/pipeline.rs::is_user_import_resolvable_error`.
//!
//! - **GAP B** (`src/lsp/completion.rs`): `extract_dot_prefix` must keep
//!   walking past matched `()` / `[]` so chained method calls and index
//!   expressions (`xs.first().`, `arr[0].`) trigger completion instead
//!   of returning an empty prefix.
//!
//! - **GAP C** (`src/lsp/document_symbols.rs`): `Decl::TraitImpl` must
//!   emit a `impl <Trait> for <Target>` symbol so trait implementations
//!   show up in the editor outline.
//!
//! All three tests drive a real `silt lsp` subprocess end-to-end over
//! LSP JSON-RPC so the full pipeline (lex → parse → typecheck → publish)
//! is exercised.

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
const READ_TIMEOUT: Duration = Duration::from_secs(15);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri(tag: &str) -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_gap_{tag}_{n}.silt")
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

    fn did_open_no_wait(&mut self, uri: &str, source: &str) {
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
    }

    /// Send a request and return the `result` field of the matching
    /// response. Drains unrelated notifications along the way.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = next_id();
        self.send_request(id, method, params);
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::from_millis(0));
            if remaining.is_zero() {
                panic!("timed out waiting for response to {method}");
            }
            match self.rx.recv_timeout(remaining) {
                Ok(msg) => {
                    if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        return msg
                            .get("result")
                            .cloned()
                            .unwrap_or(Value::Null);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    panic!("timed out waiting for response to {method}");
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
                .filter_map(|d| d.get("message").and_then(|m| m.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

// ── GAP A ───────────────────────────────────────────────────────────
//
// LSP must filter user-import cascade warnings (the "unknown module"
// warning and its follow-on "undefined variable" errors for every name
// the import brings in). Mirrors the CLI filter.

#[test]
fn lsp_diagnostics_filters_user_import_cascade_warnings() {
    let mut client = LspClient::spawn();
    client.initialize();

    let uri = unique_uri("diag_a");
    // `my_user_module` is a user-owned module the typechecker cannot
    // resolve (no filesystem lookup in this harness). The checker would
    // normally emit:
    //   warning: unknown module 'my_user_module'; ...
    //   error:   undefined variable <imported name>
    // Both must be suppressed for LSP users since the CLI filters them.
    let source = "import my_user_module\n\
                  fn main() { println(my_user_module.something()) }\n";
    let notif = client.did_open_and_wait(&uri, source);
    let messages = diagnostic_messages(&notif);

    // Exact-membership assertions (no bare .is_empty()).
    let unknown_module_hits: Vec<&String> = messages
        .iter()
        .filter(|m| m.contains("unknown module"))
        .collect();
    assert_eq!(
        unknown_module_hits,
        Vec::<&String>::new(),
        "LSP must NOT publish 'unknown module' diagnostics for user imports; \
         got: {messages:?}"
    );

    let undefined_variable_hits: Vec<&String> = messages
        .iter()
        .filter(|m| m.starts_with("undefined variable"))
        .collect();
    assert_eq!(
        undefined_variable_hits,
        Vec::<&String>::new(),
        "LSP must NOT publish 'undefined variable' cascade diagnostics for \
         user imports; got: {messages:?}"
    );

    client.shutdown();
}

// ── GAP B ───────────────────────────────────────────────────────────
//
// `extract_dot_prefix` must walk past matched parens / brackets so
// chained calls trigger completion. We exercise the full completion
// handler end-to-end: a broken `extract_dot_prefix` returns `None` for
// `xs.first().` which causes the handler to fall through to the generic
// (non-dot) completion branch, which returns *keywords* like `fn`, `let`,
// etc. A working dot-completion branch must not return keywords (even
// if the typed-AST lookup fails for the chained call — at worst the
// dot branch returns an empty array).

#[test]
fn lsp_completion_extracts_dot_prefix_after_call() {
    let mut client = LspClient::spawn();
    client.initialize();

    let uri = unique_uri("comp_b");
    // Line 2 (0-indexed 1): `    xs.first().` — cursor sits right after
    // the `.` at column 17. The `xs.first()` receiver is a postfix call
    // whose `)` used to terminate the identifier walk, short-circuiting
    // the dot-completion branch.
    let source = "fn main() {\n    let xs = [1, 2, 3]\n    xs.first().\n}\n";
    client.did_open_no_wait(&uri, source);
    // Drain the publishDiagnostics for this URI before issuing the
    // completion request so the response we pick up can only be the
    // completion response.
    let deadline = Instant::now() + READ_TIMEOUT;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::from_millis(0));
        if remaining.is_zero() {
            panic!("timed out waiting for initial publishDiagnostics");
        }
        match client.rx.recv_timeout(remaining) {
            Ok(msg) => {
                if msg.get("method").and_then(|v| v.as_str())
                    == Some("textDocument/publishDiagnostics")
                    && msg.pointer("/params/uri").and_then(|v| v.as_str()) == Some(&uri)
                {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                panic!("timed out waiting for initial publishDiagnostics");
            }
            Err(RecvTimeoutError::Disconnected) => {
                panic!("silt lsp server closed its stdout unexpectedly");
            }
        }
    }

    // Third line (0-indexed 2): `    xs.first().` — 15 chars before the
    // cursor in UTF-16 so column = 15.
    let result = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 15 },
        }),
    );
    // Completion responses can be either an array or a { items: [...] }
    // CompletionList. Extract the labels uniformly.
    let labels: Vec<String> = if let Some(arr) = result.as_array() {
        arr.iter()
            .filter_map(|it| {
                it.get("label")
                    .and_then(|l| l.as_str())
                    .map(|s| s.to_string())
            })
            .collect()
    } else if let Some(arr) = result.pointer("/items").and_then(|v| v.as_array()) {
        arr.iter()
            .filter_map(|it| {
                it.get("label")
                    .and_then(|l| l.as_str())
                    .map(|s| s.to_string())
            })
            .collect()
    } else {
        panic!(
            "unexpected completion response shape for dot-completion: {}",
            result
        );
    };

    // A working `extract_dot_prefix` sends us through the dot branch —
    // which must NOT emit keywords. A broken `extract_dot_prefix` falls
    // through to the generic branch and the response would include `fn`,
    // `let`, etc.
    //
    // Pin both the positive (no-keywords) and the kind assertions using
    // concrete strings (no bare .is_empty()).
    for kw in ["fn", "let", "match", "trait", "type", "import"] {
        assert!(
            !labels.contains(&kw.to_string()),
            "dot-completion after chained-call prefix must NOT include keyword `{kw}`; \
             got labels: {labels:?}. This means `extract_dot_prefix` bailed on the \
             closing `)` and the handler fell through to generic completion."
        );
    }
    // `println` is a builtin global only surfaced by the generic branch.
    assert!(
        !labels.contains(&"println".to_string()),
        "dot-completion after chained-call prefix must NOT include global `println`; \
         got labels: {labels:?}"
    );

    client.shutdown();
}

// ── GAP C ───────────────────────────────────────────────────────────
//
// `Decl::TraitImpl` must appear in `textDocument/documentSymbol` as a
// symbol named `impl <Trait> for <Target>`.

#[test]
fn lsp_document_symbols_includes_trait_impls() {
    let mut client = LspClient::spawn();
    client.initialize();

    let uri = unique_uri("sym_c");
    // `type Point` is a record; `trait Show for Point { ... }` is the
    // TraitImpl decl whose symbol we're asserting on.
    let source = "type Point { x: Int, y: Int }\n\
                  trait Show { fn show(self) -> String }\n\
                  trait Show for Point {\n\
                  \x20\x20fn show(self) -> String { \"p\" }\n\
                  }\n\
                  fn main() { 0 }\n";
    client.did_open_and_wait(&uri, source);

    let result = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    // documentSymbol returns either a flat SymbolInformation[] or a
    // nested DocumentSymbol[]. Our server uses the nested variant; each
    // element has a `name` field either way.
    let arr = result
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("documentSymbol should return array; got: {result}"));
    let names: Vec<String> = arr
        .iter()
        .filter_map(|s| {
            s.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    assert!(
        names.contains(&"impl Show for Point".to_string()),
        "documentSymbol must include `impl Show for Point`; got names: {names:?}"
    );

    client.shutdown();
}
