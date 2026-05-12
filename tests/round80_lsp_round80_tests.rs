//! Round-80 LSP audit fixes — regression tests.
//!
//! Two fixes covered:
//!
//! - **G1** (`src/lsp/document_symbols.rs`): `textDocument/documentSymbol`
//!   used the keyword span for both `range` and `selectionRange` on every
//!   Decl arm (Fn, Type, Trait, Let, TraitImpl). Per LSP spec, `range`
//!   should encompass the entire declaration while `selectionRange` is
//!   just the identifier. We assert that for `fn add(...)`, `type T`,
//!   `trait T`, and `let x = ...` the `selectionRange` covers ONLY the
//!   identifier columns (not the keyword) and is strictly narrower than
//!   the surrounding `range`.
//!
//! - **L4** (`src/lsp/definitions.rs::build_fn_type`): the prior
//!   implementation walked the body for every param and returned `None`
//!   if any param was unused inside the body, dropping the entire fn
//!   signature. For `fn ignore(a: Int, b: Int) -> Int = 42` (unused
//!   params) hover returned `null` and signatureHelp lost the param
//!   types. After the fix, hover renders the full `Fn(Int, Int) -> Int`
//!   signature and signatureHelp emits `a: Int, b: Int` instead of just
//!   bare names.
//!
//! Both tests drive a real `silt lsp` subprocess end-to-end over LSP
//! JSON-RPC so the full pipeline (lex → parse → typecheck → respond) is
//! exercised. Harness mirrors `tests/lsp_diagnostics_completion_symbols_tests.rs`.

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
    format!("file:///tmp/silt_round80_{tag}_{n}.silt")
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
                        return msg.get("result").cloned().unwrap_or(Value::Null);
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

fn find_symbol<'a>(symbols: &'a [Value], name: &str) -> &'a Value {
    symbols
        .iter()
        .find(|s| s.get("name").and_then(|v| v.as_str()) == Some(name))
        .unwrap_or_else(|| panic!("missing symbol '{name}' in: {symbols:?}"))
}

/// Extract a (line, character) pair from an LSP Position object.
fn pos(v: &Value) -> (u64, u64) {
    (
        v.get("line").and_then(|x| x.as_u64()).unwrap_or(0),
        v.get("character").and_then(|x| x.as_u64()).unwrap_or(0),
    )
}

// ── G1 ──────────────────────────────────────────────────────────────
//
// `selectionRange` should cover ONLY the identifier (not the keyword),
// and `range` should encompass the entire declaration (not collapse to
// the same single-token range as `selectionRange`).

#[test]
fn round80_g1_document_symbols_selection_range_is_identifier() {
    let mut client = LspClient::spawn();
    client.initialize();

    // Source layout (line/col are 0-indexed, character columns are
    // UTF-16 code units which match ASCII bytes here):
    //
    // line 0: fn add(a: Int, b: Int) -> Int = a + b
    //         01234567890123
    //                         identifier `add` lives at cols 3..6
    //         keyword `fn` at cols 0..2
    //
    // line 1: type Color { Red, Green, Blue }
    //         identifier `Color` at cols 5..10
    //
    // line 2: trait Show { fn show(self) -> String }
    //         identifier `Show` at cols 6..10
    //
    // line 3: let x = 42
    //         identifier `x` at cols 4..5
    let source = "fn add(a: Int, b: Int) -> Int = a + b\n\
                  type Color { Red, Green, Blue }\n\
                  trait Show { fn show(self) -> String }\n\
                  let x = 42\n";
    let uri = unique_uri("g1");
    client.did_open_and_wait(&uri, source);

    let result = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let symbols = result
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("documentSymbol must return array; got: {result}"));

    // ── add: selectionRange = (line 0, cols 3..6); range strictly larger
    let add = find_symbol(&symbols, "add");
    let sel = add
        .get("selectionRange")
        .expect("add must have selectionRange");
    let sel_start = pos(sel.get("start").unwrap());
    let sel_end = pos(sel.get("end").unwrap());
    assert_eq!(
        sel_start,
        (0, 3),
        "fn `add` selectionRange.start must be (line 0, col 3) — the start of the identifier, \
         not the keyword. got: {sel_start:?} (full sym: {add})"
    );
    assert_eq!(
        sel_end,
        (0, 6),
        "fn `add` selectionRange.end must be (line 0, col 6) — covering only the 3-char \
         identifier `add`. got: {sel_end:?} (full sym: {add})"
    );
    let rng = add.get("range").expect("add must have range");
    let rng_start = pos(rng.get("start").unwrap());
    let rng_end = pos(rng.get("end").unwrap());
    assert_eq!(
        rng_start,
        (0, 0),
        "fn `add` range.start must be at the keyword (line 0, col 0); got: {rng_start:?}"
    );
    assert!(
        rng_end > sel_end,
        "fn `add` range.end ({rng_end:?}) must be strictly larger than \
         selectionRange.end ({sel_end:?}) — `range` covers the whole decl, \
         `selectionRange` covers only the identifier."
    );

    // ── Color: selectionRange = (line 1, cols 5..10)
    let color = find_symbol(&symbols, "Color");
    let sel = color.get("selectionRange").unwrap();
    let sel_start = pos(sel.get("start").unwrap());
    let sel_end = pos(sel.get("end").unwrap());
    assert_eq!(
        sel_start,
        (1, 5),
        "type `Color` selectionRange.start must be the identifier start; got: {sel_start:?}"
    );
    assert_eq!(
        sel_end,
        (1, 10),
        "type `Color` selectionRange.end must end at the identifier end; got: {sel_end:?}"
    );

    // ── Show: selectionRange = (line 2, cols 6..10)
    let show = find_symbol(&symbols, "Show");
    let sel = show.get("selectionRange").unwrap();
    let sel_start = pos(sel.get("start").unwrap());
    let sel_end = pos(sel.get("end").unwrap());
    assert_eq!(
        sel_start,
        (2, 6),
        "trait `Show` selectionRange.start must be the identifier start; got: {sel_start:?}"
    );
    assert_eq!(
        sel_end,
        (2, 10),
        "trait `Show` selectionRange.end must end at the identifier end; got: {sel_end:?}"
    );

    // ── x: selectionRange = (line 3, cols 4..5)
    let x = find_symbol(&symbols, "x");
    let sel = x.get("selectionRange").unwrap();
    let sel_start = pos(sel.get("start").unwrap());
    let sel_end = pos(sel.get("end").unwrap());
    assert_eq!(
        sel_start,
        (3, 4),
        "let `x` selectionRange.start must be the identifier start; got: {sel_start:?}"
    );
    assert_eq!(
        sel_end,
        (3, 5),
        "let `x` selectionRange.end must end at the 1-char identifier end; got: {sel_end:?}"
    );

    client.shutdown();
}

// ── L4 ──────────────────────────────────────────────────────────────
//
// `build_fn_type` must NOT drop the whole fn signature when params are
// unreferenced in the body. For `fn ignore(a: Int, b: Int) -> Int = 42`,
// hover and signatureHelp must surface the full signature.

#[test]
fn round80_l4_fn_type_preserved_for_unreferenced_params() {
    let mut client = LspClient::spawn();
    client.initialize();

    // line 0: fn ignore(a: Int, b: Int) -> Int = 42
    //         identifier `ignore` lives at cols 3..9
    let source = "fn ignore(a: Int, b: Int) -> Int = 42\n\
                  fn main() { ignore(1, 2) }\n";
    let uri = unique_uri("l4");
    client.did_open_and_wait(&uri, source);

    // ── Hover on `ignore` at line 0, col 3 (start of identifier).
    let hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
        }),
    );
    assert!(
        !hover.is_null(),
        "hover on `ignore` must NOT return null — round-80 L4 regression \
         (build_fn_type was dropping the signature for unused params). got: {hover}"
    );
    let hover_text = hover
        .pointer("/contents/value")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("hover must have /contents/value; got: {hover}"));
    // The hover text must contain the rendered Fn type with both Int
    // params present. We pin the canonical render `Fn(Int, Int) -> Int`
    // which is what `Type::Fun` prints (src/types/mod.rs:111).
    assert!(
        hover_text.contains("Fn(Int, Int) -> Int"),
        "hover on `ignore` must render the full `Fn(Int, Int) -> Int` signature; \
         got: {hover_text:?}"
    );

    // ── signatureHelp at the call site `ignore(`. We position the
    // cursor immediately after the `(`. Line 1 is `fn main() { ignore(1, 2) }`.
    // The `(` after `ignore` is at column 18; cursor at column 19 sits
    // just inside the call.
    let sig = client.request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 19 },
        }),
    );
    assert!(
        !sig.is_null(),
        "signatureHelp at `ignore(` call site must return a result; got: {sig}"
    );
    let label = sig
        .pointer("/signatures/0/label")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("signatureHelp must have /signatures/0/label; got: {sig}"));
    // The signature label must include both param types. The exact
    // form produced by `build_signature_from_def` is
    // `fn ignore(a: Int, b: Int) -> Int`. We check the "Int, Int"
    // substring of the param list — without the L4 fix the label
    // collapses to `fn ignore(a, b)` (no types) because `def.ty` is
    // None, so this assertion pins the regression.
    assert!(
        label.contains("a: Int") && label.contains("b: Int"),
        "signatureHelp label must include `a: Int` and `b: Int`; got: {label:?}"
    );

    client.shutdown();
}
