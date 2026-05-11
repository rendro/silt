//! Round-84 LATENT lock: `textDocument/selectionRange` must compute a
//! tight upper bound on `expr_extent` for **non-Block** expression
//! bodies. Round 83 fixed the analogous issue for `Decl::Type` /
//! `Decl::Trait` via a dedicated `type_decl_extent` helper, but the
//! `Decl::Fn`, `Decl::Let`, and `Decl::TraitImpl` arms still flow through
//! `expr_extent`, which pre-fix collapsed to `source.len()` for any
//! `ExprKind` other than `Block`.
//!
//! Concretely:
//!
//!   * `fn add(a, b) = a + b`         — body is `Binary`, not `Block`
//!   * `let g = 99`                   — value is `Int`, not `Block`
//!   * `fn show(self) = "foo"` in an impl — body is `StringLit`, not `Block`
//!
//! All three previously claimed to extend to EOF. Selection-range chains
//! for any cursor in a later decl would drag the earlier decl's `fn`/`let`
//! span in as a parent — Shift+Alt+→ in editors cycled into the unrelated
//! function.
//!
//! Each test spawns a real `silt lsp` subprocess, opens a small source,
//! asks for the selection range at a cursor in a later decl, and asserts
//! that the chain is anchored inside the cursor's enclosing decl — not
//! at the file's first decl's keyword span.
//!
//! Pattern mirrors `tests/round83_lsp_selection_range_decl_bounds_tests.rs`.

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
                Err(RecvTimeoutError::Timeout) => panic!("timeout id={id}"),
                Err(RecvTimeoutError::Disconnected) => panic!("disconnected id={id}"),
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
                panic!("diagnostic timeout for {uri}");
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
                Err(_) => panic!("diagnostic recv error"),
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

// ── Helpers ─────────────────────────────────────────────────────────

/// Convert an LSP `Position` (line, character) into a byte offset in
/// `source`, treating each line as ASCII (column == byte offset within
/// line). All test sources here are ASCII.
fn pos_to_byte_offset(source: &str, line: u64, character: u64) -> usize {
    let mut current_line: u64 = 0;
    let mut offset: usize = 0;
    for b in source.bytes() {
        if current_line == line {
            return offset + character as usize;
        }
        if b == b'\n' {
            current_line += 1;
        }
        offset += 1;
    }
    offset
}

/// Walk the entire selection-range chain rooted at `node`, calling `f`
/// on each `SelectionRange` value (root and every parent).
fn walk_chain(node: &Value, f: &mut impl FnMut(&Value)) {
    f(node);
    if let Some(parent) = node.get("parent") {
        if !parent.is_null() {
            walk_chain(parent, f);
        }
    }
}

/// Pull `(start_line, start_char, end_line, end_char)` out of a
/// SelectionRange's `range` field. Panics on a malformed response —
/// that itself is a test failure.
fn range_quad(node: &Value) -> (u64, u64, u64, u64) {
    let r = node.get("range").expect("selection range has range field");
    (
        r.pointer("/start/line").and_then(Value::as_u64).unwrap(),
        r.pointer("/start/character")
            .and_then(Value::as_u64)
            .unwrap(),
        r.pointer("/end/line").and_then(Value::as_u64).unwrap(),
        r.pointer("/end/character").and_then(Value::as_u64).unwrap(),
    )
}

/// Compute the byte offsets `(start, end)` of an LSP range against the
/// ASCII `source`.
fn range_byte_span(source: &str, node: &Value) -> (usize, usize) {
    let (sl, sc, el, ec) = range_quad(node);
    (
        pos_to_byte_offset(source, sl, sc),
        pos_to_byte_offset(source, el, ec),
    )
}

/// Walk the chain to its outermost element (topmost ancestor with no
/// parent). LSP roots chains at the INNERMOST range; `parent` points
/// successively outward.
fn outermost(node: &Value) -> &Value {
    let mut cur = node;
    while let Some(p) = cur.get("parent") {
        if p.is_null() {
            break;
        }
        cur = p;
    }
    cur
}

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn fn_eq_expr_body_extent_does_not_extend_to_eof() {
    // Bug repro: `fn add(a, b) = a + b` — body is `Binary`, NOT `Block`.
    // Pre-fix `expr_extent` returned `source.len()` for non-Block, so the
    // `fn add` decl's span was pushed into the chain for a cursor inside
    // a later `fn main` body — outermost element of the chain anchored
    // at byte 0 (`fn` keyword of `add`), not at `fn main`.
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r84_fn_eq_expr.silt";
    let src = "fn add(a: Int, b: Int) -> Int = a + b\n\
               \n\
               fn main() {\n\
               \x20\x20let x = add(1, 2)\n\
               \x20\x20println(x)\n\
               }\n";
    client.did_open_and_wait(file, src);

    // Cursor on the `x` in `println(x)` — that's line 4 (0-indexed),
    // column 10 (`  println(x)` → byte 10 is `x`).
    let cursor_line: u64 = 4;
    let cursor_char: u64 = 10;
    assert_eq!(
        src.as_bytes()[pos_to_byte_offset(src, cursor_line, cursor_char)],
        b'x',
        "test source-shape sanity: cursor should point at `x`",
    );

    let resp = client.request(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": file },
            "positions": [ { "line": cursor_line, "character": cursor_char } ]
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("selection range result");
    assert_eq!(arr.len(), 1, "exactly one position requested → one result");
    let first = &arr[0];

    // STRONG ASSERTION 1: no element of the chain is the lone `fn`
    // keyword span of `add` (bytes 0..2). The keyword span sits at the
    // file's very start. On pre-fix code paths this was the outermost
    // chain element when the cursor lived in `fn main`.
    walk_chain(first, &mut |node| {
        let (start_off, end_off) = range_byte_span(src, node);
        assert!(
            !(start_off == 0 && end_off == 2),
            "the lone `fn` keyword span of `add` (offset 0..2) must never \
             appear in the chain when the cursor lives in `fn main`; \
             got {node:?}",
        );
    });

    // STRONG ASSERTION 2: the outermost element of the chain is anchored
    // at or after the start of `fn main` (the cursor's enclosing decl).
    let root = outermost(first);
    let (root_start, _) = range_byte_span(src, root);
    let fn_main_start = src.find("fn main").expect("`fn main` exists in source");
    assert!(
        root_start >= fn_main_start,
        "outermost chain element must be anchored in the cursor's enclosing \
         decl (`fn main` at byte {fn_main_start}); got root starting at byte \
         {root_start}: {root:?}",
    );

    client.shutdown();
}

#[test]
fn let_eq_expr_value_extent_does_not_extend_to_eof() {
    // Top-level `let g = 99` — the let's `value` is `Int(99)`, a non-Block
    // expression. Pre-fix the let-decl span was pushed for any cursor in
    // a later decl.
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r84_let_eq_expr.silt";
    let src = "let g = 99\n\
               \n\
               fn main() {\n\
               \x20\x20let x = 1\n\
               \x20\x20println(x)\n\
               }\n";
    client.did_open_and_wait(file, src);

    // Cursor on the `x` in `println(x)` — line 4, column 10.
    let cursor_line: u64 = 4;
    let cursor_char: u64 = 10;
    assert_eq!(
        src.as_bytes()[pos_to_byte_offset(src, cursor_line, cursor_char)],
        b'x',
        "test source-shape sanity: cursor should point at `x`",
    );

    let resp = client.request(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": file },
            "positions": [ { "line": cursor_line, "character": cursor_char } ]
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("selection range result");
    assert_eq!(arr.len(), 1);
    let first = &arr[0];

    // The outermost chain element MUST be anchored at or after the start
    // of `fn main`, NOT at the `let g` decl at byte 0.
    let root = outermost(first);
    let (root_start, _) = range_byte_span(src, root);
    let fn_main_start = src.find("fn main").expect("`fn main` exists in source");
    assert!(
        root_start >= fn_main_start,
        "outermost chain element must be anchored in the cursor's enclosing \
         decl (`fn main` at byte {fn_main_start}); got root starting at byte \
         {root_start}: {root:?}",
    );

    // Defensive: also check that no chain element starts at byte 0 (the
    // `let g` decl's start). This catches the case where some future
    // refactor still pushes the let-decl span but happens to NOT make it
    // outermost.
    walk_chain(first, &mut |node| {
        let (s, _e) = range_byte_span(src, node);
        assert!(
            s >= fn_main_start || s == pos_to_byte_offset(src, cursor_line, cursor_char),
            "no chain element should be anchored before `fn main` for a \
             cursor inside `fn main`; got start={s}, node={node:?}",
        );
    });

    client.shutdown();
}

#[test]
fn trait_impl_method_eq_expr_body_extent_does_not_extend_to_eof() {
    // `TraitImpl` method body written as `fn show(self) = "foo"` — body
    // is `StringLit`, a non-Block. Pre-fix the impl method's span would
    // appear in the chain for any cursor in a later decl.
    //
    // The trait `Show` is defined first so the impl typechecks; then the
    // impl with a non-Block method body; then `fn main` whose body is the
    // cursor target.
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r84_traitimpl_eq_expr.silt";
    let src = "trait Show { fn show(self) -> String }\n\
               trait Show for Int { fn show(self) = \"foo\" }\n\
               \n\
               fn main() {\n\
               \x20\x20let y = 7\n\
               \x20\x20println(y)\n\
               }\n";
    client.did_open_and_wait(file, src);

    // Cursor on the `y` in `println(y)` — line 5, column 10.
    let cursor_line: u64 = 5;
    let cursor_char: u64 = 10;
    assert_eq!(
        src.as_bytes()[pos_to_byte_offset(src, cursor_line, cursor_char)],
        b'y',
        "test source-shape sanity: cursor should point at `y`",
    );

    let resp = client.request(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": file },
            "positions": [ { "line": cursor_line, "character": cursor_char } ]
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("selection range result");
    assert_eq!(arr.len(), 1);
    let first = &arr[0];

    // The outermost chain element must be anchored at or after the start
    // of `fn main`. Pre-fix the impl method's span (or any earlier decl's
    // span dragged along via the broken extent) would land outermost.
    let root = outermost(first);
    let (root_start, _) = range_byte_span(src, root);
    let fn_main_start = src.find("fn main").expect("`fn main` exists in source");
    assert!(
        root_start >= fn_main_start,
        "outermost chain element must be anchored in the cursor's enclosing \
         decl (`fn main` at byte {fn_main_start}); got root starting at byte \
         {root_start}: {root:?}",
    );

    // No chain element should be anchored before `fn main`, period.
    walk_chain(first, &mut |node| {
        let (s, _) = range_byte_span(src, node);
        assert!(
            s >= fn_main_start,
            "no chain element should be anchored before `fn main` for a \
             cursor inside `fn main`; got start={s}, node={node:?}",
        );
    });

    client.shutdown();
}

#[test]
fn fn_eq_expr_body_extent_covers_cursor_inside_body() {
    // Positive case: cursor inside the early `fn add(a, b) = a + b` body —
    // the `fn add` decl span SHOULD appear in the chain. This verifies
    // the new tight extent doesn't UNDER-extend either.
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r84_fn_eq_expr_positive.silt";
    let src = "fn add(a: Int, b: Int) -> Int = a + b\n\
               \n\
               fn main() {\n\
               \x20\x20println(add(1, 2))\n\
               }\n";
    client.did_open_and_wait(file, src);

    // Cursor on the `+` in `a + b`: line 0, column 34.
    let cursor_line: u64 = 0;
    let cursor_char: u64 = 34;
    assert_eq!(
        src.as_bytes()[pos_to_byte_offset(src, cursor_line, cursor_char)],
        b'+',
        "test source-shape sanity: cursor should point at `+`",
    );

    let resp = client.request(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": file },
            "positions": [ { "line": cursor_line, "character": cursor_char } ]
        }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("selection range result");
    assert_eq!(arr.len(), 1);
    let first = &arr[0];

    // SOME chain element must be anchored at the `fn add` decl's start
    // (byte 0) and enclose the cursor. Round 84 widened these chain
    // elements from a bare keyword-only span to cover the whole decl
    // extent; the anchor invariant (an element starts at byte 0) is
    // preserved. This is the mirror image of the bug-repro assertion —
    // it pins that the extent does NOT under-shoot.
    let cursor_off = pos_to_byte_offset(src, cursor_line, cursor_char);
    let mut saw_decl_anchor = false;
    walk_chain(first, &mut |node| {
        let (s, e) = range_byte_span(src, node);
        if s == 0 && e >= cursor_off {
            saw_decl_anchor = true;
        }
    });
    assert!(
        saw_decl_anchor,
        "with cursor inside `fn add`'s body, some chain element should be \
         anchored at the decl's start (byte 0) and enclose the cursor (byte \
         {cursor_off}); chain={first:?}",
    );

    client.shutdown();
}
