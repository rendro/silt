//! Regression: LSP rename / goto-def on user-defined enum variants.
//!
//! `build_definitions` used to register each enum variant's `DefInfo`
//! with the enum decl's span (`t.span`), which sits on the `type`
//! KEYWORD — `ast::EnumVariant` had no name span. Consequences:
//!
//! - Renaming a variant from a usage site (`Circle(2)` -> `Disc`)
//!   emitted a TextEdit over the `type` keyword (definition site is
//!   included in the rename edit set), producing `Disc Shape { ... }`
//!   — unparseable source corruption. Same bug class as round-63 B1
//!   (`fn` keyword), round-71 DX-1 (`let`/`pub` keyword), and
//!   round-75 DX-2 (`trait` keyword).
//! - goto-definition on a variant usage landed on the `type` keyword.
//! - prepareRename/rename on the variant's own declaration line
//!   returned null (variant name spans were never visited by
//!   `find_ident_in_decl`).
//!
//! Fix mirrors the earlier keyword-clobber fixes: the parser records
//! `EnumVariant::name_span` at the variant-name token; `definitions.rs`
//! uses it for the variant `DefInfo`, and `ast_walk::find_ident_in_decl`
//! visits it so the declaration site itself resolves.
//!
//! The load-bearing gate here is parse-level: the pre-fix corruption
//! produced source that no longer parses, so after applying the
//! WorkspaceEdit we re-lex + re-parse the document and assert success.

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
    format!("file:///tmp/silt_lsp_enum_variant_rename_{tag}_{n}.silt")
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

// ── Edit helpers (mirrors round71_lsp_rename_let_and_field_tests) ─────

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
    let el = range.pointer("/end/line").and_then(|v| v.as_u64()).unwrap() as usize;
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

/// Parse-level gate: the pre-fix corruption (`Disc Shape { ... }`)
/// produced source that fails to parse, so a successful re-parse of
/// the renamed document is the authoritative "no keyword was
/// clobbered" assertion.
fn parses_ok(source: &str) -> bool {
    let Ok(tokens) = silt::lexer::Lexer::new(source).tokenize() else {
        return false;
    };
    silt::parser::Parser::new(tokens).parse_program().is_ok()
}

/// Shared fixture. 0-based layout used by the tests:
///   line 0: `type Shape {`
///   line 1: `  Circle(Int),`   (variant name `Circle` at chars 2..8)
///   line 5: `  let c = Circle(2)`  (usage `Circle` at chars 10..16)
const SOURCE: &str = "type Shape {\n  Circle(Int),\n  Square(Int),\n}\nfn main() {\n  let c = Circle(2)\n  println(1)\n}\n";

fn rename_edits_at(client: &mut LspClient, uri: &str, line: u64, character: u64) -> Vec<Value> {
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": "Disc"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on user enum variant must NOT return null; got {resp}"
    );
    result
        .get("changes")
        .and_then(|c| c.as_object())
        .expect("rename result has changes")
        .get(uri)
        .and_then(|v| v.as_array())
        .expect("file edits")
        .clone()
}

fn assert_variant_renamed_cleanly(renamed: &str) {
    // Pre-fix corruption shape: the definition TextEdit covered the
    // `type` KEYWORD (enum-decl span), producing `Disc Shape {`.
    assert!(
        renamed.starts_with("type Shape {"),
        "must not clobber the `type` keyword or enum name; got:\n{renamed}"
    );
    // The variant declaration token itself must be renamed.
    assert!(
        renamed.contains("  Disc(Int),"),
        "variant declaration should be renamed; got:\n{renamed}"
    );
    // The usage site must be renamed.
    assert!(
        renamed.contains("let c = Disc(2)"),
        "usage site should be renamed; got:\n{renamed}"
    );
    // The sibling variant must be untouched.
    assert!(
        renamed.contains("Square(Int)"),
        "sibling variant must be untouched; got:\n{renamed}"
    );
    assert!(
        !renamed.contains("Circle"),
        "old variant name must be gone; got:\n{renamed}"
    );
    // Authoritative gate: the renamed document must still PARSE.
    assert!(
        parses_ok(renamed),
        "renamed document must still parse; got:\n{renamed}"
    );
}

// ── Rename from a usage site must not clobber the `type` keyword ──────

#[test]
fn rename_enum_variant_from_usage_site_does_not_clobber_type_keyword() {
    let mut client = LspClient::spawn();
    let uri = unique_uri("usage_site");
    client.did_open_and_wait(&uri, SOURCE);

    // Cursor on `Circle` in `let c = Circle(2)`: line 5, char 10.
    let edits = rename_edits_at(&mut client, &uri, 5, 10);
    let renamed = apply_all_edits(SOURCE, &edits);
    assert_variant_renamed_cleanly(&renamed);
    client.shutdown();
}

// ── Rename on the variant's own declaration line must resolve ─────────

#[test]
fn rename_enum_variant_at_declaration_site_resolves_and_renames() {
    let mut client = LspClient::spawn();
    let uri = unique_uri("decl_site");
    client.did_open_and_wait(&uri, SOURCE);

    // prepareRename on the variant declaration (`Circle` at line 1,
    // char 2) used to return null — variant name spans were never
    // visited by `find_ident_in_decl`.
    let prep = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 2 }
        }),
    );
    let prep_result = prep.get("result").expect("prepareRename has result");
    assert!(
        !prep_result.is_null(),
        "prepareRename on a variant's own declaration must NOT return null; got {prep}"
    );

    let edits = rename_edits_at(&mut client, &uri, 1, 2);
    let renamed = apply_all_edits(SOURCE, &edits);
    assert_variant_renamed_cleanly(&renamed);
    client.shutdown();
}

// ── goto-definition on a usage must land on the variant name ──────────

#[test]
fn goto_definition_on_variant_usage_lands_on_variant_name_not_type_keyword() {
    let mut client = LspClient::spawn();
    let uri = unique_uri("goto_def");
    client.did_open_and_wait(&uri, SOURCE);

    let resp = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 5, "character": 10 }
        }),
    );
    let result = resp.get("result").expect("definition has result");
    assert!(
        !result.is_null(),
        "goto-def on variant usage must NOT return null; got {resp}"
    );
    let start_line = result
        .pointer("/range/start/line")
        .and_then(|v| v.as_u64())
        .expect("scalar Location with range");
    let start_char = result
        .pointer("/range/start/character")
        .and_then(|v| v.as_u64())
        .expect("range start character");
    // Pre-fix: landed on the `type` keyword at line 0, char 0.
    assert_eq!(
        (start_line, start_char),
        (1, 2),
        "goto-def must land on the `Circle` variant name (line 1, char 2), \
         not the `type` keyword; got {resp}"
    );
    client.shutdown();
}
