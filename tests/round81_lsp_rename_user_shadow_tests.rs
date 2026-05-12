//! Round-81 DX-G1 regression: LSP rename / prepareRename must allow
//! renaming a USER LOCAL whose lexeme happens to match a builtin name.
//!
//! Background: silt allows shadowing builtins via let-bindings, fn
//! declarations, type/trait declarations, etc. Before this fix the
//! rename gate (`is_user_renameable`) was a name-only check that
//! refused to rename anything spelled like a builtin (`Int`, `Ok`,
//! `println`, …) regardless of whether the cursor sat on a user
//! binding or a real builtin reference. A user who wrote
//! `let Int = 42` could not rename their own `Int` — the LSP refused.
//!
//! Fix: scope-aware gate. If the cursor's symbol resolves to a user
//! binding visible at this cursor (top-level def in this doc, local
//! binding identifier the cursor sits on, OR a local with this name in
//! scope), rename is allowed regardless of lexeme. Only when there is
//! NO user binding for this name in scope at the cursor do we fall
//! back to the historical `is_user_renameable` name filter.
//!
//! Mirrors the harness in `tests/lsp_rename_binding_site_tests.rs`.

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

/// (a) DX-G1 core: a user `let println = 42` should be renameable. The
/// rename request fires on the use-site (`println` in
/// `fn main() { println }`); the response must contain a non-empty
/// WorkspaceEdit covering both the binding identifier on line 0
/// (col 4..11) and the use-site on line 1 (col 12..19) — NOT a
/// "is a builtin" rejection.
///
/// NOTE on the spec example: the round-81 task description used
/// `let Int = 42` as the canonical case. silt's parser treats any
/// capitalised identifier (`Int`, `Ok`, `Some`, …) as a *constructor*
/// pattern, never a binder — so `let Int = 42` is rejected at parse
/// time before the LSP gate even runs. The DX-G1 fix is about the
/// scope-aware gate, which is equally exercised by any user binding
/// that shadows a builtin name. We use `println` (a lowercase free
/// function builtin) as the renameable shadow here. The same
/// renameability applies to `fn println` shadowing the builtin too —
/// covered by the secondary test below.
///
/// Source layout (0-based char offsets in comments, matching LSP):
///   line 0: `let println = 42`        — `println` binder at col 4..11
///   line 1: `fn main() { println }`   — `println` use-site at col 12..19
#[test]
fn rename_user_local_shadowing_builtin_int_succeeds() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_user_shadow_println.silt";
    let source = "let println = 42\nfn main() { println }\n";
    client.did_open_and_wait(uri, source);

    // Cursor on the use-site `println` at line 1, char 12.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 12 },
            "newName": "n"
        }),
    );

    // No error — the user-shadowed `println` is renameable.
    assert!(
        resp.get("error").is_none(),
        "rename of user-shadowed `println` must NOT be rejected; got {resp}"
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename of user-shadowed `println` must produce a WorkspaceEdit, not null; got {resp}"
    );

    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    assert!(
        edits.len() >= 2,
        "expected at least 2 edits (binder + use-site); got {}: {edits:?}",
        edits.len()
    );

    // Locate the binder edit (line 0) and the use-site edit (line 1).
    let mut saw_binder = false;
    let mut saw_use_site = false;
    for edit in edits {
        let line = edit.pointer("/range/start/line").and_then(|v| v.as_u64());
        let start_ch = edit
            .pointer("/range/start/character")
            .and_then(|v| v.as_u64());
        let end_ch = edit
            .pointer("/range/end/character")
            .and_then(|v| v.as_u64());
        let new_text = edit.get("newText").and_then(|v| v.as_str());
        assert_eq!(new_text, Some("n"), "edit newText must be `n`; got {edit}");
        if line == Some(0) && start_ch == Some(4) && end_ch == Some(11) {
            saw_binder = true;
        }
        if line == Some(1) && start_ch == Some(12) && end_ch == Some(19) {
            saw_use_site = true;
        }
    }
    assert!(
        saw_binder,
        "expected an edit at line 0, col 4..11 (the `let println` binder); got edits {edits:?}"
    );
    assert!(
        saw_use_site,
        "expected an edit at line 1, col 12..19 (the `println` use-site); got edits {edits:?}"
    );

    client.shutdown();
}

/// (a-bis) Same scenario, but cursor on the BINDING site of
/// `let println`. Both directions of cursor placement (binding-site vs
/// use-site) must resolve to the same user-binding renameability.
#[test]
fn rename_user_local_shadowing_builtin_int_from_binder_succeeds() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_user_shadow_println_binder.silt";
    let source = "let println = 42\nfn main() { println }\n";
    client.did_open_and_wait(uri, source);

    // Cursor on the binder `println` at line 0, char 4.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 4 },
            "newName": "n"
        }),
    );
    assert!(
        resp.get("error").is_none(),
        "rename of user-shadowed `println` (cursor on binder) must NOT be rejected; got {resp}"
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename from binder must produce a WorkspaceEdit, not null; got {resp}"
    );
    client.shutdown();
}

/// (a-ter) Sibling shadow form: `fn println(x) { x }` shadows the
/// builtin via a Decl::Fn declaration. Pre-fix this was rejected as
/// "is a builtin" because the gate was lexeme-only; post-fix the
/// scope-aware gate observes the user fn-decl in `doc.definitions`
/// and allows rename.
#[test]
fn rename_user_fn_decl_shadowing_builtin_println_succeeds() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_user_shadow_fn_println.silt";
    let source = "fn println(x) { x }\nfn main() { println(42) }\n";
    client.did_open_and_wait(uri, source);

    // Cursor on the fn-decl name `println` at line 0, char 3.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
            "newName": "n"
        }),
    );
    assert!(
        resp.get("error").is_none(),
        "rename of user `fn println` must NOT be rejected; got {resp}"
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename of user `fn println` must produce a WorkspaceEdit; got {resp}"
    );
    client.shutdown();
}

/// (b) Negative companion: a use-site of an ACTUAL builtin (no
/// user-binding shadowing in scope) must still be rejected.
///
/// Source layout:
///   line 0: `let x: Int = 42`   — `Int` is a TYPE annotation reference
///                                  to the builtin `Int`, not a binder.
///   line 1: `fn main() { println(x) }`
///
/// Two acceptable shapes (matching the gated-constructor rejection
/// test): an explicit error response, or `result: null` / empty
/// `changes` (no edits to apply). A non-empty WorkspaceEdit would mean
/// the LSP started rewriting the builtin `Int` type — that's the bug
/// we are guarding against.
///
/// Note: this test exercises the boundary where `find_ident_at_offset`
/// returns `None` for a type-annotation cursor (TypeExpr nodes are not
/// walked for rename today). The DX-G1 fix preserves that conservative
/// behaviour: no user binding for `Int` is in scope here, so the gate
/// falls through to `is_user_renameable("Int") == false` if and when
/// the cursor does resolve to the symbol.
#[test]
fn rename_use_site_of_actual_builtin_int_is_rejected() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_real_builtin_int.silt";
    let source = "let x: Int = 42\nfn main() { println(x) }\n";
    client.did_open_and_wait(uri, source);

    // Cursor on the type annotation `Int` at line 0, char 7.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 7 },
            "newName": "n"
        }),
    );

    // Acceptable rejection shapes: error set, result null, or no
    // changes (mirrors `lsp_rename_gated_constructor_rejection_tests`).
    let has_error = resp.get("error").is_some();
    let result = resp.get("result");
    let result_is_null = result.map(|v| v.is_null()).unwrap_or(true);
    let changes_empty = result
        .and_then(|r| r.get("changes"))
        .and_then(|c| c.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(true);
    assert!(
        has_error || result_is_null || changes_empty,
        "rename on real builtin `Int` must be rejected (error) or \
         produce no edits (null result / empty changes); got {resp}"
    );
    client.shutdown();
}

/// (b-bis) Stronger guard: cursor on the `println` builtin call
/// (a use-site of a builtin global with no shadowing) must be rejected
/// with the explicit "is a builtin" error message — this exercises the
/// real branch where the cursor DOES resolve to a builtin symbol.
#[test]
fn rename_use_site_of_actual_builtin_println_is_rejected_with_message() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_real_builtin_println.silt";
    let source = "fn main() { println(42) }\n";
    client.did_open_and_wait(uri, source);

    // `println` starts at line 0, char 12.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 12 },
            "newName": "n"
        }),
    );
    let err = resp.get("error").expect("expected an error response");
    let msg = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_default();
    assert!(
        msg.contains("is a builtin and cannot be renamed"),
        "expected `is a builtin and cannot be renamed` rejection; got {resp}"
    );
    client.shutdown();
}

/// (c) prepareRename complements the rename behaviour:
///   * cursor on `let println` binder ⇒ Some(Range)
///   * cursor on a real builtin reference (here, `println` use-site
///     in a doc with no user shadow) ⇒ None
///
/// Note: same parser caveat as test (a) — capital-letter pattern
/// binders are constructor patterns in silt, so we exercise the
/// scope-aware gate via a lowercase shadow (`println`).
#[test]
fn prepare_rename_user_shadow_returns_range() {
    let mut client = LspClient::spawn();
    let uri_a = "file:///tmp/silt_pr_user_shadow_println.silt";
    let uri_b = "file:///tmp/silt_pr_real_builtin_println.silt";

    client.did_open_and_wait(uri_a, "let println = 42\nfn main() { println }\n");
    client.did_open_and_wait(uri_b, "fn main() { println(42) }\n");

    // (c.1) prepareRename on the user binder `let println = 42`.
    let resp_a = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri_a },
            "position": { "line": 0, "character": 4 }
        }),
    );
    let result_a = resp_a.get("result").expect("prepareRename has result");
    assert!(
        !result_a.is_null(),
        "prepareRename on user-shadowed `let println` binder must NOT be null; got {resp_a}"
    );

    // (c.2) prepareRename on a real-builtin `println` use-site —
    // no user binding in scope, gate falls back to name-based filter
    // and rejects.
    let resp_b = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri_b },
            "position": { "line": 0, "character": 12 }
        }),
    );
    let result_b = resp_b.get("result");
    let is_null_b = result_b.map(|v| v.is_null()).unwrap_or(true);
    assert!(
        is_null_b,
        "prepareRename on real-builtin `println` use-site must be null; got {resp_b}"
    );

    client.shutdown();
}

// ── Source-grep guard ─────────────────────────────────────────────

/// Lock the refactor at the source level: rename's gate must consult
/// the new scope-aware helper, not just the lexeme-only
/// `is_user_renameable`. If a future refactor reverts to a name-only
/// gate this test fires.
#[test]
fn rename_source_uses_scope_aware_gate() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lsp/rename.rs");
    let src = std::fs::read_to_string(&path).expect("read src/lsp/rename.rs");
    assert!(
        src.contains("is_symbol_user_renameable_at_cursor"),
        "src/lsp/rename.rs must use the scope-aware gate \
         `is_symbol_user_renameable_at_cursor` so user-shadowed \
         builtin names (e.g. `let Int = 42`) are renameable. The \
         lexeme-only `is_user_renameable` is preserved as a fallback \
         (and is still public for parity tests), but it must NOT be \
         the sole gate at the rename / prepareRename call sites."
    );
    // The gate must consult local-binding scope and the per-doc
    // top-level definitions map — locking the three-arm structure of
    // the new helper.
    assert!(
        src.contains("find_local_binding_at_offset") && src.contains("nearest_local_binding_for"),
        "scope-aware rename gate must consult `find_local_binding_at_offset` \
         (cursor on a local binder) AND `nearest_local_binding_for` (use-site \
         of a local in scope)."
    );
    assert!(
        src.contains("doc.definitions.contains_key"),
        "scope-aware rename gate must check `doc.definitions` for top-level \
         user definitions (so `let Int = 42` at module scope shadows the \
         builtin and becomes renameable)."
    );
}
