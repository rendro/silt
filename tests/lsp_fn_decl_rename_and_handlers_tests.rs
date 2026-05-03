//! Round-63 regression: LSP rename / references / definition /
//! documentHighlight on `fn` and `type` declaration names.
//!
//! - **B1** — `definitions::build_definitions` previously stored the
//!   keyword span (`fn`/`type`) instead of the name span. LSP rename
//!   then replaced the keyword with the new name, producing
//!   unparseable code (e.g. `renamed helper() { 0 }`). The fix adds a
//!   `name_span` field on `FnDecl`/`TypeDecl` that the parser fills in
//!   from the identifier token; the LSP `DefInfo` now records that
//!   span instead.
//!
//! - **B2** — The four handlers `references`, `definition`,
//!   `documentHighlight`, and `implementation` called the
//!   non-source-aware `find_ident_at_offset`, so a cursor on a `fn`
//!   declaration name silently no-op'd (`null` response) — even though
//!   `rename` and `hover` already used the source-aware variant. The
//!   fix wires the four handlers to `find_ident_at_offset_with_source`
//!   so the binding-site walk on `fn` decl names is reachable.

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
    format!("file:///tmp/silt_lsp_r63_fndecl_{tag}_{n}.silt")
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

// ── Helpers ────────────────────────────────────────────────────────

/// Apply a single LSP `TextEdit` to a string buffer. `range.start` /
/// `range.end` are LSP {line, character} positions over UTF-16 code
/// units; for ASCII source (the test fixtures we use) this is the same
/// as character offsets.
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
    // Compute byte offsets for (sl, sc) and (el, ec).
    let mut start_off = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if i == sl {
            // Add `sc` chars within this line — assume ASCII.
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

/// Apply every edit in a LSP `WorkspaceEdit#changes` map for a given
/// URI to a source string. Edits are sorted in reverse document order
/// so earlier edits don't shift later ones.
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
        // Reverse-sort so we apply bottom-up.
        key(b).cmp(&key(a))
    });
    let mut out = source.to_string();
    for e in &sorted {
        out = apply_edit(&out, e);
    }
    out
}

/// Parse silt source via the public lib API. Returns true iff parsing
/// produces no errors.
fn parses_clean(source: &str) -> bool {
    let tokens = match silt::lexer::Lexer::new(source).tokenize() {
        Ok(t) => t,
        Err(_) => return false,
    };
    let (_program, errs) = silt::parser::Parser::new(tokens).parse_program_recovering();
    errs.is_empty()
}

// ── B1: rename produces parseable result ───────────────────────────

#[test]
fn rename_fn_decl_produces_parseable_result() {
    // Cursor on `helper` declaration name. Pre-fix the rename returned
    // an edit on the `fn` keyword (chars 0..2), which corrupted the
    // program to `renamed helper() { 0 }\nfn main() { renamed() }` —
    // unparseable, plus the original `helper` decl was not renamed.
    let source = "fn helper() { 0 }\nfn main() { helper() }\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("rename_fn_parses");
    client.did_open_and_wait(&uri, source);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
            "newName": "renamed"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on fn-decl name must NOT return null; got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(&uri)
        .and_then(|v| v.as_array())
        .expect("file edits");

    let renamed = apply_all_edits(source, edits);
    assert!(
        parses_clean(&renamed),
        "rename result must parse cleanly; got:\n{renamed}"
    );
    assert!(
        renamed.contains("fn renamed()"),
        "rename should produce `fn renamed()`; got:\n{renamed}"
    );
    assert!(
        renamed.contains("renamed()"),
        "use site should be renamed; got:\n{renamed}"
    );
    assert!(
        !renamed.contains("fn helper"),
        "old name must be gone from decl; got:\n{renamed}"
    );
    // Specifically guard against the pre-fix corruption shape, in which
    // the `fn` keyword on the helper decl was replaced.
    assert!(
        !renamed.starts_with("renamed helper"),
        "must not corrupt the `fn` keyword; got:\n{renamed}"
    );
    client.shutdown();
}

#[test]
fn rename_type_decl_produces_parseable_result() {
    // Cursor on `Color` declaration name in `type Color { Red, Green, Blue }`.
    // Pre-fix: rename overwrote the `type` keyword span, producing an
    // unparseable header.
    let source = "type Color {\n  Red,\n  Green,\n  Blue,\n}\nfn main() { Red }\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("rename_type_parses");
    client.did_open_and_wait(&uri, source);

    // Cursor on `Color`: line 0, char=5 (after `type `).
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 5 },
            "newName": "Hue"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on type-decl name must NOT return null; got {resp}"
    );
    let changes = result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes");
    let edits = changes
        .get(&uri)
        .and_then(|v| v.as_array())
        .expect("file edits");

    let renamed = apply_all_edits(source, edits);
    assert!(
        parses_clean(&renamed),
        "rename result must parse cleanly; got:\n{renamed}"
    );
    assert!(
        renamed.contains("type Hue"),
        "rename should produce `type Hue`; got:\n{renamed}"
    );
    assert!(
        !renamed.contains("type Color"),
        "old name must be gone; got:\n{renamed}"
    );
    // Pre-fix corruption: the keyword `type` would have been replaced.
    assert!(
        !renamed.starts_with("Hue Color"),
        "must not corrupt the `type` keyword; got:\n{renamed}"
    );
    client.shutdown();
}

// ── B2: handlers respond on fn-decl binding site ───────────────────

#[test]
fn references_on_fn_decl_returns_array() {
    // Cursor on `helper` declaration name. Pre-fix the handler returned
    // null because `find_ident_at_offset` did not walk fn-decl names.
    let source = "fn helper() { 0 }\nfn main() { helper() }\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("refs_fn_decl");
    client.did_open_and_wait(&uri, source);

    let resp = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
            "context": { "includeDeclaration": true }
        }),
    );
    let result = resp.get("result").expect("references has result");
    assert!(
        !result.is_null(),
        "references on fn-decl name must NOT be null; got {resp}"
    );
    let arr = result.as_array().expect("references is array");
    assert!(
        arr.len() >= 2,
        "expected at least 2 references (decl + use); got {}: {arr:?}",
        arr.len()
    );

    // Decl site is at line 0; use site is on line 1.
    let lines: Vec<u64> = arr
        .iter()
        .filter_map(|loc| loc.pointer("/range/start/line").and_then(|v| v.as_u64()))
        .collect();
    assert!(
        lines.iter().any(|&l| l == 0),
        "expected decl-site reference on line 0; got lines {lines:?}"
    );
    assert!(
        lines.iter().any(|&l| l == 1),
        "expected use-site reference on line 1; got lines {lines:?}"
    );
    client.shutdown();
}

#[test]
fn definition_on_fn_decl_self_reference() {
    // Cursor on `helper` declaration name. The definition handler
    // should resolve the symbol and return the decl site itself.
    let source = "fn helper() { 0 }\nfn main() { helper() }\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("def_fn_decl");
    client.did_open_and_wait(&uri, source);

    let resp = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 }
        }),
    );
    let result = resp.get("result").expect("definition has result");
    assert!(
        !result.is_null(),
        "definition on fn-decl name must NOT be null; got {resp}"
    );
    client.shutdown();
}

#[test]
fn document_highlight_on_fn_decl_returns_both_sites() {
    // Cursor on `helper` declaration name. Should highlight both the
    // decl and the call site.
    let source = "fn helper() { 0 }\nfn main() { helper() }\n";
    let mut client = LspClient::spawn();
    let uri = unique_uri("highlight_fn_decl");
    client.did_open_and_wait(&uri, source);

    let resp = client.request(
        "textDocument/documentHighlight",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 }
        }),
    );
    let result = resp.get("result").expect("documentHighlight has result");
    assert!(
        !result.is_null(),
        "documentHighlight on fn-decl name must NOT be null; got {resp}"
    );
    let arr = result.as_array().expect("highlight is array");
    assert!(
        arr.len() >= 2,
        "expected at least 2 highlights (decl + use); got {}: {arr:?}",
        arr.len()
    );
    let lines: Vec<u64> = arr
        .iter()
        .filter_map(|h| h.pointer("/range/start/line").and_then(|v| v.as_u64()))
        .collect();
    assert!(
        lines.iter().any(|&l| l == 0),
        "expected highlight on decl line 0; got {lines:?}"
    );
    assert!(
        lines.iter().any(|&l| l == 1),
        "expected highlight on use line 1; got {lines:?}"
    );
    client.shutdown();
}
