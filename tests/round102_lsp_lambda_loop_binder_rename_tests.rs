//! Round-102 BROKEN: the LSP rename/references walker never visited
//! lambda PARAMETER BINDERS (or their type annotations) and `loop`
//! binding binders.
//!
//! `workspace::collect_references_in_expr` had no `ExprKind::Lambda`
//! arm, and its `_` fallback (`visit_expr_children`) walks only the
//! lambda BODY. So renaming a lambda param from a body use-site edited
//! the uses but not the `fn(n)` / `{ n -> ... }` binder token — the
//! applied WorkspaceEdit produced `fn(n) { m * 2 }`, which fails
//! `silt check` with an undefined variable. `textDocument/references`
//! likewise omitted the binder, and renaming a user type missed lambda
//! param annotations (`fn(p: Point) { ... }`) because `Param.ty` was
//! never walked. Same class: `ExprKind::Loop { bindings, .. }` binders
//! (`loop acc = 0`) carry only a `Symbol` (no span) and were never
//! collected, so body uses were renamed while the binder was not.
//!
//! Lock: drive the live LSP server over stdio, rename from a body
//! use-site, assert the WorkspaceEdit covers BOTH the binder token and
//! the uses, apply it, and gate on the full front end (lex + parse +
//! typecheck) — pre-fix, the applied text failed with an undefined
//! variable / unknown type.
//!
//! Harness mirrors `tests/round101_lsp_type_position_rename_tests.rs`.

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
/// errors, and typecheck cleanly. The bug produced WorkspaceEdits whose
/// application yielded undefined-variable / unknown-type errors, i.e.
/// this function returning an Err.
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

fn rename_edits(client: &mut LspClient, uri: &str, pos: (u64, u64), new_name: &str) -> Vec<Value> {
    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": pos.0, "character": pos.1 },
            "newName": new_name
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(!result.is_null(), "rename must not return null; got {resp}");
    result
        .pointer(&format!("/changes/{uri}"))
        .or_else(|| result.get("changes").and_then(|c| c.get(uri)))
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("rename result must contain edits for {uri}; got {resp}"))
        .clone()
}

// ── Tests ──────────────────────────────────────────────────────────

/// `fn(n) { n * 2 }`: rename from the body USE must also edit the
/// `fn(n)` binder token. Pre-fix: only the use was edited, and the
/// applied text (`fn(n) { m * 2 }`) failed the front end.
const FN_LAMBDA_SRC: &str = "fn apply(f, x) { f(x) }\n\
                             \n\
                             fn main() {\n  \
                             let doubled = apply(fn(n) { n * 2 }, 3)\n  \
                             println(\"{doubled}\")\n\
                             }\n";

#[test]
fn rename_fn_lambda_param_from_body_use_edits_binder_and_use() {
    front_end_errors(FN_LAMBDA_SRC).expect("fixture must lex/parse/typecheck cleanly");

    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r102_rn_fn_lambda_param.silt";
    client.did_open_and_wait(uri, FN_LAMBDA_SRC);

    // Cursor on the `n` of `n * 2` (body use-site).
    let use_pos = pos_of(FN_LAMBDA_SRC, "n * 2", 0);
    let edits = rename_edits(&mut client, uri, use_pos, "m");

    assert_eq!(
        edits.len(),
        2,
        "expected exactly 2 edits (the `fn(n)` binder + the `n * 2` use); got {edits:#?}"
    );

    let applied = apply_edits(FN_LAMBDA_SRC, &edits);
    assert!(
        applied.contains("fn(m) { m * 2 }"),
        "binder AND use must be renamed; got:\n{applied}"
    );
    front_end_errors(&applied).unwrap_or_else(|e| {
        panic!("applying the rename edit must yield a compiling program; {e}\n---\n{applied}")
    });
    client.shutdown();
}

#[test]
fn references_on_lambda_param_use_include_binder() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r102_refs_lambda_param.silt";
    client.did_open_and_wait(uri, FN_LAMBDA_SRC);

    let use_pos = pos_of(FN_LAMBDA_SRC, "n * 2", 0);
    let resp = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": use_pos.0, "character": use_pos.1 },
            "context": { "includeDeclaration": true }
        }),
    );
    let refs = resp
        .get("result")
        .and_then(|r| r.as_array())
        .unwrap_or_else(|| panic!("references must return an array; got {resp}"));

    // Binder `n` in `fn(n)`: the char right after `fn(`.
    let binder_pos = {
        let (l, c) = pos_of(FN_LAMBDA_SRC, "fn(n)", 0);
        (l, c + 3)
    };
    let has_binder = refs.iter().any(|loc| {
        loc.pointer("/range/start/line").and_then(|v| v.as_u64()) == Some(binder_pos.0)
            && loc
                .pointer("/range/start/character")
                .and_then(|v| v.as_u64())
                == Some(binder_pos.1)
    });
    assert!(
        has_binder,
        "references must include the `fn(n)` binder at {binder_pos:?}; got {refs:#?}"
    );
    client.shutdown();
}

/// Trailing-closure form `{ n -> n * 2 }` (the shape used all over the
/// README/docs): rename from the body use must edit the `n ->` binder.
const TRAILING_CLOSURE_SRC: &str = "fn apply(x, f) { f(x) }\n\
                                    \n\
                                    fn main() {\n  \
                                    let r = apply(3) { n -> n * 2 }\n  \
                                    println(\"{r}\")\n\
                                    }\n";

#[test]
fn rename_trailing_closure_param_from_body_use_edits_binder_and_use() {
    front_end_errors(TRAILING_CLOSURE_SRC).expect("fixture must lex/parse/typecheck cleanly");

    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r102_rn_trailing_closure_param.silt";
    client.did_open_and_wait(uri, TRAILING_CLOSURE_SRC);

    let use_pos = pos_of(TRAILING_CLOSURE_SRC, "n * 2", 0);
    let edits = rename_edits(&mut client, uri, use_pos, "m");

    assert_eq!(
        edits.len(),
        2,
        "expected exactly 2 edits (the `{{ n ->` binder + the `n * 2` use); got {edits:#?}"
    );

    let applied = apply_edits(TRAILING_CLOSURE_SRC, &edits);
    assert!(
        applied.contains("{ m -> m * 2 }"),
        "binder AND use must be renamed; got:\n{applied}"
    );
    front_end_errors(&applied).unwrap_or_else(|e| {
        panic!("applying the rename edit must yield a compiling program; {e}\n---\n{applied}")
    });
    client.shutdown();
}

/// Renaming a user TYPE must rewrite lambda param annotations
/// (`fn(p: Point) { ... }`) — `Param.ty` was never walked for lambdas.
const TYPE_ANNOTATION_SRC: &str = "type Point { x: Int, y: Int }\n\
                                   \n\
                                   fn main() {\n  \
                                   let getx = fn(p: Point) { p.x }\n  \
                                   let pt = Point { x: 7, y: 1 }\n  \
                                   let gx = getx(pt)\n  \
                                   println(\"{gx}\")\n\
                                   }\n";

#[test]
fn rename_type_updates_lambda_param_annotation() {
    front_end_errors(TYPE_ANNOTATION_SRC).expect("fixture must lex/parse/typecheck cleanly");

    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r102_rn_lambda_param_annotation.silt";
    client.did_open_and_wait(uri, TYPE_ANNOTATION_SRC);

    // Rename at the DECLARATION name (`Point` in `type Point`).
    let decl_pos = pos_of(TYPE_ANNOTATION_SRC, "Point", 0);
    let edits = rename_edits(&mut client, uri, decl_pos, "Pt");

    // Exactly 3 references: decl + lambda param annotation +
    // construction head. Pre-fix: 2 (the annotation was missing).
    assert_eq!(
        edits.len(),
        3,
        "expected 3 edits (decl, `fn(p: Point)` annotation, `Point {{ x: 7` head); got {edits:#?}"
    );

    let applied = apply_edits(TYPE_ANNOTATION_SRC, &edits);
    assert!(
        applied.contains("fn(p: Pt) { p.x }"),
        "lambda param annotation must be renamed; got:\n{applied}"
    );
    assert!(
        !applied.contains("Point"),
        "no `Point` reference may survive the rename; got:\n{applied}"
    );
    front_end_errors(&applied).unwrap_or_else(|e| {
        panic!("applying the rename edit must yield a compiling program; {e}\n---\n{applied}")
    });
    client.shutdown();
}

/// `loop acc = 0 { ... }`: the binder Symbol carries no span, so it was
/// never collected — body uses got renamed, the binder token did not.
const LOOP_SRC: &str = "fn main() {\n  \
                        let total = loop acc = 0 {\n    \
                        match acc < 5 {\n      \
                        true -> loop(acc + 1)\n      \
                        _ -> acc\n    \
                        }\n  \
                        }\n  \
                        println(\"{total}\")\n\
                        }\n";

#[test]
fn rename_loop_binder_from_body_use_edits_binder_and_uses() {
    front_end_errors(LOOP_SRC).expect("fixture must lex/parse/typecheck cleanly");

    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r102_rn_loop_binder.silt";
    client.did_open_and_wait(uri, LOOP_SRC);

    // Cursor on the `acc` of `acc < 5` (body use-site).
    let use_pos = pos_of(LOOP_SRC, "acc < 5", 0);
    let edits = rename_edits(&mut client, uri, use_pos, "count");

    // Exactly 4 references: the `loop acc = 0` binder + 3 body uses.
    // Pre-fix: 3 (the binder was missing).
    assert_eq!(
        edits.len(),
        4,
        "expected 4 edits (`loop acc = 0` binder + `acc < 5` + `loop(acc + 1)` + `-> acc`); \
         got {edits:#?}"
    );

    let binder_pos = pos_of(LOOP_SRC, "acc = 0", 0);
    let has_binder = edits.iter().any(|e| {
        e.pointer("/range/start/line").and_then(|v| v.as_u64()) == Some(binder_pos.0)
            && e.pointer("/range/start/character").and_then(|v| v.as_u64()) == Some(binder_pos.1)
    });
    assert!(
        has_binder,
        "edits must include the `loop acc = 0` binder at {binder_pos:?}; got {edits:#?}"
    );

    let applied = apply_edits(LOOP_SRC, &edits);
    assert!(
        applied.contains("loop count = 0 {"),
        "loop binder must be renamed; got:\n{applied}"
    );
    assert!(
        applied.contains("loop(count + 1)"),
        "loop re-entry use must be renamed; got:\n{applied}"
    );
    front_end_errors(&applied).unwrap_or_else(|e| {
        panic!("applying the rename edit must yield a compiling program; {e}\n---\n{applied}")
    });
    client.shutdown();
}
