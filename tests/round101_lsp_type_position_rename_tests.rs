//! Round-101 BROKEN: LSP rename / references / goto-definition were
//! blind to TYPE-POSITION references. Renaming a user type from its
//! declaration produced exactly ONE edit (the decl name) and silently
//! broke the program: every annotation (`p: Point`), return type
//! (`-> Point`), record-field type (`inner: Point`), construction head
//! (`Point { x: 3 }`), and pattern head (`Point { x, .. }`) kept the
//! old name, so `silt check` failed with `unknown type 'Point'` after
//! applying the WorkspaceEdit.
//!
//! Root cause: `workspace::collect_references_in_expr` matched only
//! `ExprKind::Ident`; `collect_references_in_decl` never walked
//! `TypeExpr` trees; `collect_references_in_pattern` ignored
//! Constructor/Record head names. `ast_walk::find_ident_in_decl` had
//! the same blind spot, so definition/prepareRename couldn't even
//! resolve type-position cursors.
//!
//! Lock: drive the live LSP server over stdio, rename the type at its
//! DECLARATION, apply the returned WorkspaceEdit to the source, and
//! assert the result still passes the full parse + typecheck (the bug
//! is in the LSP's edit output, so `check` on the applied text is the
//! right gate), plus per-site containment assertions. A decl-only
//! edit-count assertion alone would be a weak gate.
//!
//! Harness mirrors `tests/round100_lsp_rename_record_shorthand_binder_tests.rs`.

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
                Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for response id={id}"),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("server disconnected waiting for id={id}")
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
                Err(RecvTimeoutError::Timeout) => panic!("diagnostic timeout for {uri}"),
                Err(RecvTimeoutError::Disconnected) => panic!("server disconnected"),
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
        self.send_raw(&json!({"jsonrpc": "2.0", "id": id, "method": "shutdown"}));
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

/// Apply LSP single-line TextEdits to `text` and return the result.
fn apply_edits(text: &str, edits: &[Value]) -> String {
    let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
    // Apply each edit; sort so later positions are applied first to keep
    // earlier offsets valid. All edits here are single-line.
    let mut sorted: Vec<&Value> = edits.iter().collect();
    sorted.sort_by_key(|e| {
        let line = e
            .pointer("/range/start/line")
            .and_then(|v| v.as_u64())
            .unwrap();
        let ch = e
            .pointer("/range/start/character")
            .and_then(|v| v.as_u64())
            .unwrap();
        std::cmp::Reverse((line, ch))
    });
    for e in sorted {
        let line = e
            .pointer("/range/start/line")
            .and_then(|v| v.as_u64())
            .unwrap() as usize;
        let sc = e
            .pointer("/range/start/character")
            .and_then(|v| v.as_u64())
            .unwrap() as usize;
        let ec = e
            .pointer("/range/end/character")
            .and_then(|v| v.as_u64())
            .unwrap() as usize;
        let new_text = e.get("newText").and_then(|v| v.as_str()).unwrap();
        let l = &lines[line];
        lines[line] = format!("{}{}{}", &l[..sc], new_text, &l[ec..]);
    }
    lines.join("\n")
}

/// (0-based line, 0-based char) of the `occurrence`-th (0-based) match
/// of `needle` in `text`. All-ASCII sources only.
fn pos_of(text: &str, needle: &str, occurrence: usize) -> (u64, u64) {
    let mut search_from = 0usize;
    let mut off = None;
    for _ in 0..=occurrence {
        let found = text[search_from..]
            .find(needle)
            .unwrap_or_else(|| panic!("needle {needle:?} (occurrence {occurrence}) not in text"));
        off = Some(search_from + found);
        search_from += found + 1;
    }
    let off = off.unwrap();
    let line = text[..off].bytes().filter(|&b| b == b'\n').count() as u64;
    let line_start = text[..off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    (line, (off - line_start) as u64)
}

/// Full front-end gate: the source must lex, parse without recovery
/// errors, and typecheck cleanly. This is the load-bearing assertion —
/// the round-101 bug produced WorkspaceEdits whose application yielded
/// `unknown type` errors, i.e. this function returning an Err.
fn front_end_errors(source: &str) -> Result<(), String> {
    let tokens = silt::lexer::Lexer::new(source)
        .tokenize()
        .map_err(|e| format!("lex error: {e:?}"))?;
    let (mut program, parse_errors) = silt::parser::Parser::new(tokens).parse_program_recovering();
    if !parse_errors.is_empty() {
        return Err(format!("parse errors: {parse_errors:?}"));
    }
    let type_errors: Vec<_> = silt::typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == silt::types::Severity::Error)
        .collect();
    if !type_errors.is_empty() {
        return Err(format!("type errors: {type_errors:?}"));
    }
    Ok(())
}

/// One `Point` reference per type-position class:
///   line 0: declaration            `type Point { ... }`
///   line 1: record-field type      `inner: Point`
///   line 2: param annotation       `p: Point`
///   line 3: return type + construction head `-> Point { Point { ... } }`
///   line 5: stmt-let annotation    `let p: Point = ...`
///   line 7: pattern head           `match p { Point { x, .. } -> ... }`
const SOURCE: &str = "type Point { x: Int, y: Int }\n\
                      type Wrap { inner: Point }\n\
                      fn dist(p: Point) -> Int { p.x * p.x + p.y * p.y }\n\
                      fn mk(n: Int) -> Point { Point { x: n, y: n } }\n\
                      fn main() {\n  \
                      let p: Point = mk(3)\n  \
                      let d = dist(p)\n  \
                      match p { Point { x, .. } -> println(\"{x} {d}\") }\n\
                      }\n";

#[test]
fn rename_type_from_decl_updates_all_type_position_references() {
    // Sanity: the fixture itself must be a clean program, otherwise the
    // post-rename gate below would be vacuous.
    front_end_errors(SOURCE).expect("fixture must lex/parse/typecheck cleanly");

    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r101_rn_type_pos.silt";
    client.did_open_and_wait(uri, SOURCE);

    // Rename at the DECLARATION name (`Point` in `type Point`).
    let (line, ch) = pos_of(SOURCE, "Point", 0);
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": ch },
            "newName": "Pt"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on a type decl must not return null; got {resp}"
    );
    let edits = result
        .pointer(&format!("/changes/{uri}"))
        .or_else(|| result.get("changes").and_then(|c| c.get(uri)))
        .and_then(|v| v.as_array())
        .expect("file edits");

    // Exactly 7 references: decl + field-type + param annotation +
    // return type + construction head + stmt-let annotation + pattern
    // head. Pre-fix the response contained exactly ONE edit (the decl).
    assert_eq!(
        edits.len(),
        7,
        "expected 7 edits (decl, `inner: Point`, `p: Point`, `-> Point`, \
         `Point {{ x: n`, `let p: Point`, pattern `Point {{ x, ..`); got {edits:#?}"
    );

    let applied = apply_edits(SOURCE, edits);
    for expected in [
        "type Pt { x: Int, y: Int }",
        "type Wrap { inner: Pt }",
        "fn dist(p: Pt) -> Int",
        "fn mk(n: Int) -> Pt { Pt { x: n, y: n } }",
        "let p: Pt = mk(3)",
        "match p { Pt { x, .. } ->",
    ] {
        assert!(
            applied.contains(expected),
            "renamed source must contain {expected:?}; got:\n{applied}"
        );
    }
    assert!(
        !applied.contains("Point"),
        "no `Point` reference may survive the rename; got:\n{applied}"
    );

    // Load-bearing gate: the program produced by APPLYING the LSP's
    // WorkspaceEdit must still pass the full front end. Pre-fix this
    // failed with `unknown type 'Point'` at every non-decl site.
    front_end_errors(&applied).unwrap_or_else(|e| {
        panic!("applying the rename edit must yield a compiling program; {e}\n---\n{applied}")
    });
    client.shutdown();
}

#[test]
fn type_position_cursors_resolve_for_definition_references_prepare_rename() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r101_type_pos_nav.silt";
    client.did_open_and_wait(uri, SOURCE);

    // (a) goto-definition on the `Point` in the `p: Point` annotation
    // must land on the declaration (line 0). Pre-fix: `null`.
    let (ann_line, ann_ch) = {
        let (l, c) = pos_of(SOURCE, "p: Point", 0);
        (l, c + 3)
    };
    let resp = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": ann_line, "character": ann_ch }
        }),
    );
    let result = resp.get("result").expect("definition has result");
    assert!(
        !result.is_null(),
        "goto-definition on a type annotation must not return null; got {resp}"
    );
    let def_line = result
        .pointer("/range/start/line")
        .or_else(|| result.pointer("/0/range/start/line"))
        .and_then(|v| v.as_u64());
    assert_eq!(
        def_line,
        Some(0),
        "definition of `Point` must be the decl on line 0; got {resp}"
    );

    // (b) references from the annotation cursor must cover the decl and
    // the other type-position sites. Pre-fix: `null`.
    let resp = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": ann_line, "character": ann_ch },
            "context": { "includeDeclaration": true }
        }),
    );
    let refs = resp
        .get("result")
        .and_then(|r| r.as_array())
        .unwrap_or_else(|| {
            panic!("references on a type annotation must return an array; got {resp}")
        });
    assert!(
        refs.len() >= 7,
        "expected >= 7 references (decl + 6 type-position sites); got {}: {refs:#?}",
        refs.len()
    );

    // (c) prepareRename on the construction head `Point {{ x: n ... }}`
    // must offer a range. Pre-fix: `null`.
    let (con_line, con_ch) = pos_of(SOURCE, "Point { x: n", 0);
    let resp = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": con_line, "character": con_ch + 1 }
        }),
    );
    let result = resp.get("result").expect("prepareRename has result");
    assert!(
        !result.is_null(),
        "prepareRename on a record-construction head must not return null; got {resp}"
    );
    client.shutdown();
}
