//! Regression tests for round-26 audit findings L8 and G5.
//!
//! L8 — `module::builtin_module_constants` must enumerate every constant
//!      registered on a builtin module in `src/typechecker/builtins.rs`.
//!      Before the fix, it only listed `math.{pi,e}` and silently omitted
//!      all seven `float.*` constants (`max_value`, `min_value`, `epsilon`,
//!      `min_positive`, `infinity`, `neg_infinity`, `nan`).
//!
//! G5 — LSP dot-completion (`textDocument/completion` after `math.` / `float.`)
//!      must surface those constants as completion items alongside the
//!      module's functions. Before the fix, `dot_completions` in
//!      `src/lsp.rs` only enumerated `builtin_module_functions` and never
//!      consulted `builtin_module_constants`, so editor autocompletion
//!      failed to suggest `math.pi`, `float.infinity`, etc.
//!
//! The L8 checks call into `silt::module` directly. The G5 checks spawn
//! the compiled `silt lsp` binary and drive it over stdin/stdout using
//! the same framing helpers as `tests/lsp.rs`, because `dot_completions`
//! is a private method on the LSP server struct.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use silt::module;

// ── L8: direct tests against `builtin_module_constants` ───────────

#[test]
fn math_constants_are_listed() {
    // Regression: `math.pi` and `math.e` must stay in the list after
    // we extend it to cover `float.*` constants.
    let consts = module::builtin_module_constants("math");
    assert!(
        consts.contains(&"pi"),
        "expected `math.pi` in builtin_module_constants(\"math\"), got: {consts:?}"
    );
    assert!(
        consts.contains(&"e"),
        "expected `math.e` in builtin_module_constants(\"math\"), got: {consts:?}"
    );
}

#[test]
fn float_constants_are_listed() {
    // L8 fix: all seven float constants registered by
    // `TypeChecker::register_float_builtins` in src/typechecker/builtins.rs
    // must appear in `builtin_module_constants("float")`. Prior to the fix
    // the function returned an empty Vec for "float".
    let consts = module::builtin_module_constants("float");
    for expected in [
        "max_value",
        "min_value",
        "epsilon",
        "min_positive",
        "infinity",
        "neg_infinity",
        "nan",
    ] {
        assert!(
            consts.contains(&expected),
            "expected `float.{expected}` in builtin_module_constants(\"float\"), got: {consts:?}"
        );
    }
}

#[test]
fn unknown_module_has_no_constants() {
    // Control: modules without registered constants return an empty Vec
    // (not a panic, not a fallback list). This guards against someone
    // adding a catch-all arm to the match statement.
    assert!(
        module::builtin_module_constants("not_a_real_module").is_empty(),
        "unknown module must return an empty constants list"
    );
    // A builtin module with no registered constants (e.g. `string`) must
    // also return an empty list.
    assert!(
        module::builtin_module_constants("string").is_empty(),
        "string has no registered constants but returned non-empty list"
    );
}

#[test]
fn float_functions_do_not_duplicate_constants() {
    // After the L8 fix, `min_value` / `max_value` live in
    // `builtin_module_constants("float")`, not in
    // `builtin_module_functions("float")`. Keeping them in both lists
    // would produce duplicate LSP completion items and mislabel the
    // constants as functions in the type signature map.
    let fns = module::builtin_module_functions("float");
    for c in module::builtin_module_constants("float") {
        assert!(
            !fns.contains(&c),
            "`float.{c}` must not appear in both the function and constant lists, fns={fns:?}"
        );
    }
}

// ── G5: LSP subprocess harness ─────────────────────────────────────
//
// The following helpers mirror `tests/lsp.rs` — driving `silt lsp` as a
// subprocess over stdin/stdout is the least-invasive way to exercise
// `dot_completions` end-to-end (the method itself is private on the
// server struct).

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_module_constants_test_{n}.silt")
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
                Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for response id={id}"),
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
                Ok(None) => thread::sleep(Duration::from_millis(20)),
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

/// Ask the server for completions at (line, character) in a freshly-opened
/// document and return the list of `label` strings from the response.
fn dot_completion_labels(source: &str, line: u32, character: u32) -> Vec<String> {
    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    client.did_open_and_wait(&uri, source);

    let id = next_id();
    client.send_request(
        id,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
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
        "expected non-null completion result at a valid dot-completion position"
    );

    let items: Vec<Value> = if let Some(arr) = result.as_array() {
        arr.clone()
    } else if let Some(arr) = result.pointer("/items").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        panic!("unexpected completion result shape: {result}");
    };

    let labels = items
        .iter()
        .filter_map(|it| it.get("label").and_then(|v| v.as_str()).map(String::from))
        .collect();

    client.shutdown();
    labels
}

// ── G5: LSP dot-completion surfaces module constants ───────────────

#[test]
fn lsp_dot_completion_after_math_includes_pi_and_e() {
    // Source has the cursor placed right after the dot in `math.` on line 2.
    //   line 0: import math
    //   line 1: fn main() {
    //   line 2:   math.
    //   line 3: }
    // "  math." is 7 characters, so the cursor sits at column 7.
    let source = "import math\nfn main() {\n  math.\n}\n";
    let labels = dot_completion_labels(source, 2, 7);

    assert!(
        labels.iter().any(|l| l == "pi"),
        "expected `pi` in dot-completion labels for `math.`, got: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == "e"),
        "expected `e` in dot-completion labels for `math.`, got: {labels:?}"
    );
}

#[test]
fn lsp_dot_completion_after_math_still_includes_functions() {
    // Regression check for G5: adding constants to the completion list
    // must not remove the existing function completions.
    let source = "import math\nfn main() {\n  math.\n}\n";
    let labels = dot_completion_labels(source, 2, 7);

    for expected in ["sin", "cos", "sqrt", "pow"] {
        assert!(
            labels.iter().any(|l| l == expected),
            "expected `math.{expected}` to still appear in dot-completion, got: {labels:?}"
        );
    }
}

#[test]
fn lsp_dot_completion_after_float_includes_all_constants() {
    // `float.` must surface every constant registered in
    // src/typechecker/builtins.rs. "  float." is 8 characters.
    let source = "import float\nfn main() {\n  float.\n}\n";
    let labels = dot_completion_labels(source, 2, 8);

    for expected in [
        "max_value",
        "min_value",
        "epsilon",
        "min_positive",
        "infinity",
        "neg_infinity",
        "nan",
    ] {
        assert!(
            labels.iter().any(|l| l == expected),
            "expected `float.{expected}` in dot-completion labels, got: {labels:?}"
        );
    }
}

#[test]
fn lsp_dot_completion_after_float_still_includes_functions() {
    // Regression check: float.parse / float.round / etc. must keep showing up.
    let source = "import float\nfn main() {\n  float.\n}\n";
    let labels = dot_completion_labels(source, 2, 8);

    for expected in ["parse", "round", "abs", "to_string"] {
        assert!(
            labels.iter().any(|l| l == expected),
            "expected `float.{expected}` to still appear in dot-completion, got: {labels:?}"
        );
    }
}
