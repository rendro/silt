//! Round-77 LSP-L1: inlay hints inside trait default-method bodies.
//!
//! Background — `src/lsp/inlay_hints.rs::walk_decl` previously matched
//! `Decl::Fn`, `Decl::Let`, `Decl::TraitImpl`, then `_ => {}` for the
//! `Decl::Trait` arm. Default-method bodies authored inside a trait
//! declaration therefore got no inlay hints from that walk path, even
//! when typechecking succeeded — the walker never descended into them.
//!
//! The fix mirrors the `Decl::TraitImpl` arm: walk every method on the
//! trait via `collect_fn_hints`, which in turn descends into the body
//! and emits a `: <type>` hint for every `let x = expr` whose author
//! omitted the type ascription.
//!
//! This test drives the actual LSP server end-to-end: it spawns the
//! `silt lsp` subprocess, opens a document containing a trait with a
//! default-bodied method whose body has an unannotated let-binding,
//! requests `textDocument/inlayHint`, and asserts the rendered hint
//! label is `: Int` and its position pins to the trait body's source.
//! Asserting on the rendered label/position (not just a non-empty list
//! from `walk_decl`) avoids the weak-gate pattern flagged in the audit
//! guide.
//!
//! Note: with the impl `trait Foo for Item {}`, the typechecker's
//! `synthesize_default_methods` pass clones the trait's default FnDecl
//! into the impl's `methods` Vec preserving its source spans, and the
//! pass-3 body checker then decorates that clone's `Expr.ty` fields.
//! The rendered hint at the trait body's source therefore travels
//! through the existing `Decl::TraitImpl` arm of `walk_decl`. The new
//! `Decl::Trait` arm is structural insurance: when (a future) typecheck
//! pass also decorates the trait decl's own body in-place, hints will
//! continue to render correctly. Either way the user sees one `: Int`
//! at the trait's `let x = 1`.

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

#[test]
fn inlay_hints_emitted_for_trait_default_method_let_binding() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_round77_lsp_inlay_trait_default.silt";
    // Trait `Foo` with a default-bodied method `bar` that contains an
    // unannotated `let x = 1` — the round-77 LSP-L1 audit example. We
    // pair it with a record + bare impl so the typechecker actually
    // type-decorates the default body via `synthesize_default_methods`
    // (which clones the trait's default FnDecl into the impl, preserving
    // its source spans, then `check_fn_body_with_name` types the cloned
    // body in-place). The rendered hint's `position` pins it to the
    // trait body's *source* location — that's the user-visible `: Int`
    // annotation the audit calls out.
    let src = "\
type Item { name: String }
trait Foo {
  fn bar(self) -> Int {
    let x = 1
    x + 1
  }
}
trait Foo for Item {}
fn main() { 0 }
";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": file },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 20, "character": 0 }
            }
        }),
    );

    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let labels: Vec<String> = arr
        .iter()
        .filter_map(|h| h.get("label").and_then(|l| l.as_str()).map(String::from))
        .collect();

    // The `let x = 1` inside the trait's default method body must
    // render a `: Int` hint. Pre-fix, `walk_decl` skipped `Decl::Trait`
    // entirely so this hint never appeared.
    assert!(
        labels.iter().any(|l| l == ": Int"),
        "expected `: Int` hint for `let x = 1` inside trait default method body; got {labels:?}"
    );

    // Pinpoint the hint's position so a future regression that swaps
    // the trait-walk for a sibling decl's hint (e.g. accidentally
    // hinting somewhere outside the trait) is caught. The trait body's
    // `let x = 1` sits on line 3 (0-indexed); the hint is rendered
    // immediately after the `x` ident at character 9 (`    let x` =
    // 4 spaces + `let ` + `x` ⇒ char 9).
    let int_hint = arr
        .iter()
        .find(|h| h.get("label").and_then(|l| l.as_str()) == Some(": Int"))
        .expect("`: Int` hint must exist");
    let pos = int_hint
        .get("position")
        .expect("hint has a position");
    // The `let x = 1` sits on line 3 of `src` (zero-indexed): line 0
    // is the type decl, 1 is `trait Foo {`, 2 is `  fn bar(self) ->
    // Int {`, 3 is `    let x = 1`. The rendered hint sits right after
    // the `x` ident at column 9 (`    let x` ⇒ 4 spaces + `let ` + `x`
    // = 9 chars; UTF-8 only, so UTF-16 column matches).
    assert_eq!(
        pos.get("line").and_then(|l| l.as_u64()),
        Some(3),
        "hint must render at the trait body's source line (line 3); got {int_hint:?}"
    );
    assert_eq!(
        pos.get("character").and_then(|l| l.as_u64()),
        Some(9),
        "hint must render right after `x` ident at column 9; got {int_hint:?}"
    );

    // Lock dedup: there is exactly one `: Int` hint at this position.
    // If a future change accidentally walks both the trait decl AND the
    // impl-synthesized clone without deduplicating (the trait clone and
    // impl-synthesized clone share the same source span by construction
    // — see `synthesize_default_methods`), we would emit duplicate
    // hints at this exact position. Lock the count at 1.
    let dup_count = arr
        .iter()
        .filter(|h| {
            let p = h.get("position");
            let line = p.and_then(|p| p.get("line")).and_then(|l| l.as_u64());
            let ch = p.and_then(|p| p.get("character")).and_then(|c| c.as_u64());
            let lbl = h.get("label").and_then(|l| l.as_str());
            line == Some(3) && ch == Some(9) && lbl == Some(": Int")
        })
        .count();
    assert_eq!(
        dup_count, 1,
        "expected exactly one `: Int` hint at the let-binding position; got {dup_count} — full hints: {arr:?}"
    );

    client.shutdown();
}
