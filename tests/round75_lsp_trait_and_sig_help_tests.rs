//! Round-75 regression tests for LSP trait rename / references and
//! signature-help bracket-aware comma counting.
//!
//! - **DX-1** — `scan_call_site_forward` only tracked `(`/`)` parens.
//!   Commas inside list/record/map literals and blocks were credited
//!   to the enclosing call's argument count, so `foo([1, 2], cursor)`
//!   reported `active_param=2` instead of `1`. Fix: extend the stack
//!   to also push/pop on `[`/`]` and `{`/`}` so commas at non-paren
//!   nesting levels are not counted.
//!
//! - **DX-2** — `TraitDecl` lacked `name_span`; `definitions.rs`
//!   recorded `t.span` (the `trait` keyword) as the trait's
//!   `DefInfo.span`. LSP rename then replaced the `trait` keyword with
//!   the new name — exactly the same bug round-71 fixed for `let`,
//!   round-63 fixed for `fn`/`type`, but missed for `trait`.
//!
//! - **DX-4** — `TraitImpl::trait_name` and `TraitImpl::target_type`
//!   were never visited by the LSP ident walker, so `find_ident` /
//!   `references` / `rename` skipped both references inside the impl
//!   header. Where-clause trait references and supertrait references
//!   on `TraitDecl` were also unvisited.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
static URI_COUNTER: AtomicU64 = AtomicU64::new(1);
const READ_TIMEOUT: Duration = Duration::from_secs(15);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri(tag: &str) -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_r75_trait_{tag}_{n}.silt")
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

// ── Edit helpers ──────────────────────────────────────────────────

fn apply_edit(source: &str, edit: &Value) -> String {
    let new_text = edit
        .get("newText")
        .and_then(|v| v.as_str())
        .expect("edit has newText");
    let range = edit.get("range").expect("edit has range");
    let sl = range
        .pointer("/start/line")
        .and_then(|v| v.as_u64())
        .unwrap() as usize;
    let sc = range
        .pointer("/start/character")
        .and_then(|v| v.as_u64())
        .unwrap() as usize;
    let el = range
        .pointer("/end/line")
        .and_then(|v| v.as_u64())
        .unwrap() as usize;
    let ec = range
        .pointer("/end/character")
        .and_then(|v| v.as_u64())
        .unwrap() as usize;

    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut start_off = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if i == sl {
            start_off += sc.min(line.len());
            break;
        }
        start_off += line.len();
    }
    let mut end_off = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if i == el {
            end_off += ec.min(line.len());
            break;
        }
        end_off += line.len();
    }

    let mut buf = String::with_capacity(source.len() + new_text.len());
    buf.push_str(&source[..start_off]);
    buf.push_str(new_text);
    buf.push_str(&source[end_off..]);
    buf
}

fn apply_all_edits(source: &str, edits: &[Value]) -> String {
    let mut sorted: Vec<Value> = edits.to_vec();
    sorted.sort_by(|a, b| {
        let key = |e: &Value| -> (u64, u64) {
            (
                e.pointer("/range/start/line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                e.pointer("/range/start/character")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            )
        };
        key(b).cmp(&key(a))
    });
    let mut out = source.to_string();
    for e in &sorted {
        out = apply_edit(&out, e);
    }
    out
}

// ── DX-1: signature-help bracket-aware comma counting ─────────────

#[test]
fn sig_help_bracket_inner_comma_not_credited_to_call() {
    // `fn foo(a: List(Int), b: Int) {}` — at the cursor right before
    // the closing `)`, expect active_parameter = 1 (we're on `b`),
    // NOT 2 (which would happen if the comma inside `[1, 2]` were
    // counted as a separator at the call site).
    let source = "fn foo(a: List(Int), b: Int) -> Int {\n  b\n}\nfn main() {\n  let _r = foo([1, 2], 3)\n}\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("sig_bracket");
    client.did_open_and_wait(&uri, source);

    // Line index 4: `  let _r = foo([1, 2], 3)`
    // Columns:        0         1         2
    //                 0123456789012345678901234
    // The `3` sits at column 23 (after `[1, 2], `). Cursor at column
    // 23 puts us "on b", and the active_parameter must be 1.
    let resp = client.request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 4, "character": 23 }
        }),
    );

    let result = resp
        .get("result")
        .expect("signatureHelp response has result");
    assert!(!result.is_null(), "expected non-null sig-help result");

    let sigs = result
        .pointer("/signatures")
        .and_then(|v| v.as_array())
        .expect("signatures array present");
    assert!(!sigs.is_empty(), "must contain at least one signature");

    let active = sigs[0]
        .get("activeParameter")
        .and_then(|v| v.as_u64())
        .expect("SignatureInformation has activeParameter");
    assert_eq!(
        active, 1,
        "active parameter must be 1 (b) — comma inside `[1, 2]` must NOT be counted as a call-site separator; got: {sigs:?}"
    );

    client.shutdown();
}

// ── DX-2: rename of trait declaration name does not clobber `trait`
//          keyword. ────────────────────────────────────────────────

#[test]
fn rename_trait_decl_does_not_clobber_trait_keyword() {
    // Pre-fix: `trait Foo { fn bar(self) -> Int = 0 }` after rename
    // `Foo` -> `Bar` produced `Bar Foo { ... }` because the trait
    // decl's `DefInfo.span` was the `trait` keyword.
    let source = "trait Foo {\n  fn bar(self) -> Int = 0\n}\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("rename_trait_decl");
    client.did_open_and_wait(&uri, source);

    // Cursor on `Foo`: line 0, char 6 (after `trait `).
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 6 },
            "newName": "Bar"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on trait-decl name must NOT return null; got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename has changes");
    let edits = changes
        .get(&uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    let renamed = apply_all_edits(source, edits);

    assert!(
        renamed.contains("trait Bar {"),
        "rename should produce `trait Bar {{`; got:\n{renamed}"
    );
    // Pre-fix corruption shape:
    assert!(
        !renamed.starts_with("Bar Foo"),
        "must not corrupt the `trait` keyword; got:\n{renamed}"
    );
    assert!(
        !renamed.contains("trait Foo"),
        "old trait name must be gone from decl; got:\n{renamed}"
    );

    client.shutdown();
}

// ── DX-4: trait-impl trait_name + target_type + where-clause refs are
//          renamed when the trait is renamed. ──────────────────────

#[test]
fn rename_trait_updates_impl_header_and_where_clause() {
    // Source uses `trait Greet { ... }` (the declaration), one impl
    // `trait Greet for Int { ... }`, and a where-clause reference
    // `where n: Greet`. Renaming the trait `Greet -> Wave` from the
    // declaration site must update:
    //   1. the trait declaration itself,
    //   2. the impl-header `trait Greet for Int`,
    //   3. the where-clause's `Greet` reference.
    //
    // Pre-fix: ast_walk + workspace did not visit the impl's
    // `trait_name` or any where-clause `trait_name`, so the rename
    // only edited the declaration and left both impl and where-clause
    // references stale.
    let source = "trait Greet {\n  fn hello(self) -> String\n}\n\
                  trait Greet for Int {\n  fn hello(self) -> String = \"hi\"\n}\n\
                  fn x(n: a) -> String where a: Greet {\n  n.hello()\n}\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("rename_trait_xref");
    client.did_open_and_wait(&uri, source);

    // Cursor on `Greet` in the trait decl: line 0, char 6.
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 6 },
            "newName": "Wave"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on trait-decl name must NOT return null; got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename has changes");
    let edits = changes
        .get(&uri)
        .and_then(|v| v.as_array())
        .expect("file edits");
    let renamed = apply_all_edits(source, edits);

    assert!(
        renamed.contains("trait Wave {"),
        "trait declaration must be renamed; got:\n{renamed}"
    );
    assert!(
        renamed.contains("trait Wave for Int"),
        "impl-header trait_name must be renamed; got:\n{renamed}"
    );
    assert!(
        renamed.contains("where a: Wave"),
        "where-clause trait-ref must be renamed; got:\n{renamed}"
    );
    assert!(
        !renamed.contains("Greet"),
        "no `Greet` remnant should remain; got:\n{renamed}"
    );

    client.shutdown();
}
