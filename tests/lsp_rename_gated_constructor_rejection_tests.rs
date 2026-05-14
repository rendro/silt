//! Round-60 L5 regression: behavioural LSP test that rename on a
//! gated builtin constructor (e.g. `IoNotFound`) is rejected.
//!
//! `tests/builtin_constructor_parity_tests.rs::lsp_rename_covers_every_gated_constructor`
//! already enforces, at the source-grep level, that `rename.rs`
//! consults `module::all_builtin_constructor_names`. This test locks
//! the same property end-to-end through the LSP transport so that
//! future refactors to the rename pipeline can't silently regress
//! the rejection behaviour.
//!
//! Mirrors the harness in `tests/lsp_workspace_tests.rs`.

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

/// Behaviourally lock the rename rejection on a gated constructor.
/// The current correct behaviour returns either an LSP error response
/// (preferred — `is_user_renameable` rejects builtin constructors) or
/// an empty/null `result` (no edits to apply). Either is acceptable as
/// long as no `WorkspaceEdit` with non-empty `changes` comes back.
#[test]
fn rename_on_gated_constructor_is_rejected() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_gated_ctor.silt";
    // `IoNotFound` is a gated constructor under `module::io`. We use
    // it as a name in a context the parser will accept (a reference
    // mention) so the typechecker doesn't reject it before rename runs.
    // Even with a parse/typecheck error the rename pipeline is driven
    // by AST tokens, so `IoNotFound` mentioned in source is enough for
    // `find_ident_at_offset` to surface the symbol to the rename guard.
    let source = "fn main() {\n  let x = IoNotFound\n  x\n}\n";
    client.did_open_and_wait(uri, source);

    // `IoNotFound` starts at line=1, char=10.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 10 },
            "newName": "RenamedCtor"
        }),
    );

    // Tightened (round 86 L4): the server MUST explicitly respond.
    // Either an `error` is set (preferred — `is_user_renameable`
    // rejects the builtin constructor and the rename handler returns
    // `Err(Response::new_err(...))`), or the server responded with a
    // present `result` that is null / has empty changes. A bare `{}`
    // (no `result` and no `error` — e.g. stub server stopped
    // responding) must NOT pass this assertion.
    let has_error = resp.get("error").is_some();
    let result = resp.get("result");
    let result_present = result.is_some();
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(false);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);

    assert!(
        has_error || (result_present && (result_is_null || changes_empty)),
        "rename on gated constructor `IoNotFound` must be explicitly \
         rejected (error response) or explicitly empty (present null \
         result / present empty changes); a missing-both shape (no \
         `error`, no `result`) means the server did not respond — got \
         {resp}"
    );
    client.shutdown();
}

/// Negative companion: rename on a *user-defined* identifier in a
/// program that ALSO mentions `IoNotFound` succeeds. This locks the
/// finer-grained behaviour: the rejection must apply only to the
/// builtin, not blanket-block the document.
#[test]
fn rename_on_user_ident_in_doc_mentioning_gated_ctor_succeeds() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_user_in_gated_doc.silt";
    let source = "fn renamable_fn() { 0 }\nfn main() { let _ = IoNotFound\n  renamable_fn() }\n";
    client.did_open_and_wait(uri, source);

    // Cursor on `renamable_fn` call site at line 2, char=2.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 2 },
            "newName": "fresh_user_fn"
        }),
    );
    let result = resp.get("result").expect("rename result present");
    assert!(
        !result.is_null(),
        "rename on user-defined fn must succeed even when doc mentions a gated constructor; got {resp}"
    );
    // Round 79 follow-up: round-60 L5 sibling (above) calls
    // `client.shutdown()` at the end. This test was missing the
    // call, so on Windows runners nextest flagged the LSP child
    // process as a leak (PID 2500) and the job exited 1 even
    // though the assertion itself passed. Shut down explicitly to
    // match the sibling and clear the runner's leak detector.
    client.shutdown();
}

/// Lock for round 86 L4: the tightened rename-rejection assertion
/// shape must NOT accept a degenerate `{}` response (server stopped
/// responding — no `error`, no `result`). Without this guard, the
/// previous `result_is_null = ...unwrap_or(true)` / `changes_empty =
/// ...unwrap_or(true)` made every "no response" case pass silently.
///
/// This unit test exercises the same boolean expression used in
/// `rename_on_gated_constructor_is_rejected` and
/// `round81_lsp_rename_user_shadow_tests::rename_use_site_of_actual_builtin_int_is_rejected`,
/// against a `{}` value. If anyone relaxes that expression back to
/// the old shape, this test fires.
#[test]
fn tightened_rename_rejection_predicate_rejects_empty_object() {
    use serde_json::json;

    let resp = json!({}); // no `error`, no `result`

    let has_error = resp.get("error").is_some();
    let result = resp.get("result");
    let result_present = result.is_some();
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(false);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);

    let tightened_passes = has_error || (result_present && (result_is_null || changes_empty));
    assert!(
        !tightened_passes,
        "tightened rejection predicate must REJECT a bare {{}} response \
         (no `error`, no `result`); otherwise a non-responding server \
         silently 'passes' the rename-rejection tests"
    );

    // Sanity: the predicate should still accept the three legitimate
    // rejection shapes so the integration tests it backs do not
    // regress.
    let with_error = json!({"error": {"code": -32602, "message": "x"}});
    let has_error = with_error.get("error").is_some();
    let result = with_error.get("result");
    let result_present = result.is_some();
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(false);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);
    assert!(
        has_error || (result_present && (result_is_null || changes_empty)),
        "an explicit error response must still pass the tightened predicate"
    );

    let with_null_result = json!({"result": null});
    let has_error = with_null_result.get("error").is_some();
    let result = with_null_result.get("result");
    let result_present = result.is_some();
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(false);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);
    assert!(
        has_error || (result_present && (result_is_null || changes_empty)),
        "an explicit null result must still pass the tightened predicate"
    );

    let with_empty_changes = json!({"result": {"changes": {}}});
    let has_error = with_empty_changes.get("error").is_some();
    let result = with_empty_changes.get("result");
    let result_present = result.is_some();
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(false);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);
    assert!(
        has_error || (result_present && (result_is_null || changes_empty)),
        "an explicit empty-changes result must still pass the tightened predicate"
    );
}
