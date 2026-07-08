//! Round-101 BROKEN: `textDocument/rename` involving a record SHORTHAND
//! binder that sits AFTER a nested braced sub-pattern corrupted the source.
//!
//! The round-100 fix (76987f8) resolved the flat case (`Point { x, y }`)
//! by scanning from the pattern head to the FIRST `}` for the field token.
//! But a nested braced sub-pattern before the binder — `Point { a: Inner
//! { y }, x }` — closes that scan at the `}` of `Inner { y }`, so the
//! binder `x` was never found and:
//!
//!   * `workspace.rs::shorthand_binder_span` returned `None`, whose
//!     fallback pushed the record HEAD span — rename rewrote `Point` into
//!     the new name while leaving the binder `x` untouched (doubly
//!     broken, non-compiling output);
//!   * `ast_walk.rs::check_shorthand_field_binder` could not resolve the
//!     binder under the cursor, so prepareRename/hover/goto-def were
//!     blind to it.
//!
//! Additionally, match-arm PATTERNS were never walked by the reference
//! collector at all (`visit_expr_children` only visits scrutinee, guards
//! and bodies), so a match-arm binder rename edited the body uses but not
//! the binder.
//!
//! Fix under test:
//!   * `text_utils::find_shorthand_binder` — brace-depth-aware scan that
//!     only matches direct fields of the record's own braces;
//!   * the `None` fallback now emits NO edit instead of the head span;
//!   * `collect_references_in_expr` walks match-arm patterns.
//!
//! These tests drive the live LSP server over stdio (the real execution
//! path), apply the returned WorkspaceEdit, and assert the binder IS
//! edited, the constructor head is NOT, and the applied result still
//! lexes and parses (see `assert_parses` for why the gate is not a full
//! typecheck). Harness mirrors
//! `tests/round100_lsp_rename_record_shorthand_binder_tests.rs`.

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

/// The applied rename result must still lex and parse — the pre-fix bug
/// clobbered the constructor head token, corrupting the pattern's very
/// shape. Deliberately NOT a full typecheck gate: renaming a SHORTHAND
/// binder (`{ x }`) to a name that is not a field of the record
/// legitimately fails the typechecker's field check afterwards ("record
/// 'Point' has no field 'w'") — the binder name IS the field name in
/// shorthand form, and the LSP performs a token-level rename by design.
/// The round-100 flat-case lock
/// (tests/round100_lsp_rename_record_shorthand_binder_tests.rs) draws
/// the same line: it asserts the edited strings, not typechecking. The
/// string-containment assertions in each test below pin the semantic
/// outcome; this gate pins structural integrity.
fn assert_parses(applied: &str) {
    let tokens = silt::lexer::Lexer::new(applied)
        .tokenize()
        .unwrap_or_else(|e| panic!("applied rename result no longer lexes: {e:?}\n{applied}"));
    silt::parser::Parser::new(tokens)
        .parse_program()
        .unwrap_or_else(|e| panic!("applied rename result no longer parses: {e:?}\n{applied}"));
}

/// Exact repro from the round-101 finding: rename `x` (from its use inside
/// the interpolation) where the shorthand binder `x` follows the nested
/// braced sub-pattern `Inner { y }` inside a MATCH-ARM pattern.
///
/// Before the fix the first-`}` scan stopped at the brace closing
/// `Inner { y }`, the head-span fallback fired, and the WorkspaceEdit
/// rewrote `Point` while leaving the binder `x` untouched.
#[test]
fn rename_shorthand_binder_after_nested_subpattern_in_match_arm() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r101_rn_nested_match.silt";
    // Line 6: `    Point { a: Inner { y }, x } -> println("{x + y}")`
    //          head `Point` at char 4, binder `x` at char 28, use at 45.
    let text = "type Inner { y: Int }\n\
                type Point { a: Inner, x: Int }\n\
                \n\
                fn main() {\n  \
                let p = Point { a: Inner { y: 2 }, x: 1 }\n  \
                match p {\n    \
                Point { a: Inner { y }, x } -> println(\"{x + y}\")\n  \
                }\n\
                }\n";
    client.did_open_and_wait(uri, text);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 6, "character": 45 },
            "newName": "w"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on a use of a nested-shorthand binder must not return null; got {resp}"
    );
    let edits = result
        .pointer(&format!("/changes/{uri}"))
        .or_else(|| result.get("changes").and_then(|c| c.get(uri)))
        .and_then(|v| v.as_array())
        .expect("file edits");

    // (a) The binder `x` token (line 6, char 28) IS edited.
    assert!(
        edits.iter().any(|e| {
            e.pointer("/range/start/line").and_then(|v| v.as_u64()) == Some(6)
                && e.pointer("/range/start/character").and_then(|v| v.as_u64()) == Some(28)
        }),
        "the shorthand binder `x` (line 6, char 28) must receive an edit; got {edits:?}"
    );
    // (b) The `Point` head token (line 6, char 4) is NOT edited — nor is
    // anything else before the record's own fields.
    assert!(
        edits.iter().all(|e| {
            e.pointer("/range/start/line").and_then(|v| v.as_u64()) != Some(6)
                || e.pointer("/range/start/character").and_then(|v| v.as_u64()) >= Some(28)
        }),
        "no edit may touch the `Point` pattern head (line 6, char 4); got {edits:?}"
    );

    // (c) Applying every edit renames binder + use, keeps everything else,
    // and the result still typechecks.
    let applied = apply_edits(text, edits);
    assert!(
        applied.contains("Point { a: Inner { y }, w } -> println(\"{w + y}\")"),
        "binder and use must be renamed with `Point`/`Inner` intact; got:\n{applied}"
    );
    assert!(
        applied.contains("let p = Point { a: Inner { y: 2 }, x: 1 }"),
        "the record-literal `x: 1` field label must be untouched; got:\n{applied}"
    );
    assert!(
        !applied.contains("w { a: Inner"),
        "the constructor head must NOT be clobbered; got:\n{applied}"
    );
    assert_parses(&applied);
    client.shutdown();
}

/// Same hole through the `let`-destructure path (walked by the reference
/// collector since round 62): the shorthand binder after a nested braced
/// sub-pattern must be edited, the head must survive.
#[test]
fn rename_shorthand_binder_after_nested_subpattern_in_let_destructure() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r101_rn_nested_let.silt";
    // Line 4: `  let Point { a: Inner { y }, x } = Point { a: Inner { y: 2 }, x: 1 }`
    //          head `Point` at char 6, binder `x` at char 30.
    // Line 5: `  println("{x + y}")` — use `x` at char 12.
    let text = "type Inner { y: Int }\n\
                type Point { a: Inner, x: Int }\n\
                \n\
                fn main() {\n  \
                let Point { a: Inner { y }, x } = Point { a: Inner { y: 2 }, x: 1 }\n  \
                println(\"{x + y}\")\n\
                }\n";
    client.did_open_and_wait(uri, text);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 5, "character": 12 },
            "newName": "w"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on a use of a nested-shorthand binder must not return null; got {resp}"
    );
    let edits = result
        .pointer(&format!("/changes/{uri}"))
        .or_else(|| result.get("changes").and_then(|c| c.get(uri)))
        .and_then(|v| v.as_array())
        .expect("file edits");

    let applied = apply_edits(text, edits);
    assert!(
        applied.contains("let Point { a: Inner { y }, w } = Point { a: Inner { y: 2 }, x: 1 }"),
        "binder renamed, heads and the RHS `x: 1` field label intact; got:\n{applied}"
    );
    assert!(
        applied.contains("println(\"{w + y}\")"),
        "the binder's use inside the interpolation must be renamed; got:\n{applied}"
    );
    assert!(
        !applied.contains("w { a: Inner"),
        "the constructor head must NOT be clobbered; got:\n{applied}"
    );
    assert_parses(&applied);
    client.shutdown();
}

/// Sibling assertion from the finding: prepareRename with the cursor ON
/// the shorthand binder that follows a nested braced sub-pattern must
/// resolve (the same truncated scan blinded
/// `ast_walk::check_shorthand_field_binder`, so hover / goto-def /
/// prepareRename returned nothing for the binder).
#[test]
fn prepare_rename_resolves_shorthand_binder_after_nested_subpattern() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r101_prep_nested.silt";
    let text = "type Inner { y: Int }\n\
                type Point { a: Inner, x: Int }\n\
                \n\
                fn main() {\n  \
                let p = Point { a: Inner { y: 2 }, x: 1 }\n  \
                match p {\n    \
                Point { a: Inner { y }, x } -> println(\"{x + y}\")\n  \
                }\n\
                }\n";
    client.did_open_and_wait(uri, text);

    // Cursor ON the binder `x` (line 6, char 28).
    let resp = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 6, "character": 28 }
        }),
    );
    let result = resp.get("result").expect("prepareRename has result");
    assert!(
        !result.is_null(),
        "prepareRename on a shorthand binder after a nested braced \
         sub-pattern must resolve it (not null); got {resp}"
    );
    client.shutdown();
}
