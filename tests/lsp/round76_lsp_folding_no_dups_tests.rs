//! Round-76 D1: foldingRange must not emit duplicate folds for fn /
//! trait-method / trait-impl-method bodies.
//!
//! Pre-fix: `collect_decl_folds` pushed a fold for the body block via
//! `push_block_fold(&f.body.span, &f.body, ...)` AND then called
//! `walk_expr_folds(&f.body, ...)`, which itself recognises the
//! `ExprKind::Block` body and pushes the same fold again. Trait /
//! TraitImpl method bodies followed the same broken pattern. The
//! existing `lsp_tier2_tests::folding_range_covers_fn_body` only
//! asserted "at least one fold" so duplicates slipped through.
//!
//! Lock: real LSP `textDocument/foldingRange` request, asserting the
//! exact number of folds for a controlled input.
//!   * Two top-level fns → exactly 2 folds (was 4).
//!   * Trait + impl with two methods each → all-distinct (start,end)
//!     tuples (pre-fix every method body produced two folds with the
//!     same span).

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

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn folding_range_two_fns_emits_exactly_two_folds() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r76_fold_two_fns.silt";
    // Two single-line fn bodies (each spanning lines 0-2 / 3-5). Pre-fix
    // we would see 4 folds; post-fix exactly 2.
    let src = "fn a() {\n  1\n}\nfn b() {\n  2\n}\n";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": file } }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("folding range result");
    assert_eq!(
        arr.len(),
        2,
        "expected exactly 2 folds (one per fn body); got {} — {arr:?}",
        arr.len()
    );

    // Also confirm the fold spans are distinct (no two folds with the
    // same start_line — which would indicate a true duplicate).
    let starts: Vec<u64> = arr
        .iter()
        .filter_map(|f| f.get("startLine").and_then(|l| l.as_u64()))
        .collect();
    let mut deduped = starts.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(
        starts.len(),
        deduped.len(),
        "duplicate folds detected (same startLine reported twice): {starts:?}"
    );

    client.shutdown();
}

#[test]
fn folding_range_trait_impl_two_methods_emits_three_folds() {
    let mut client = LspClient::spawn();
    let file = "file:///tmp/silt_r76_fold_trait_impl.silt";
    // Trait header + impl with two methods. The impl's body fold + one
    // fold per method body. Pre-fix the impl would yield 1 + 2*2 = 5
    // folds for the impl alone (each method body double-pushed) plus
    // the trait span fold. Post-fix exactly 1 (trait) + 1 (impl) + 2
    // (method bodies) = 4 folds. We assert that no two folds share the
    // same (startLine, endLine) — that's the hallmark of the
    // duplication bug.
    let src = "\
trait T {
  fn a(self) -> Int
  fn b(self) -> Int
}
type W { v: Int }
trait T for W {
  fn a(self) -> Int {
    self.v
  }
  fn b(self) -> Int {
    self.v + 1
  }
}
fn main() {
  let w = W { v: 1 }
  w.a()
}
";
    client.did_open_and_wait(file, src);

    let resp = client.request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": file } }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .expect("folding range result");

    // Each fold has a distinct (startLine, endLine) tuple. The duplicate
    // bug produced two identical folds for every method body; deduping
    // by (start, end) and comparing to the original length detects it.
    let folds: Vec<(u64, u64)> = arr
        .iter()
        .map(|f| {
            (
                f.get("startLine").and_then(|l| l.as_u64()).unwrap_or(0),
                f.get("endLine").and_then(|l| l.as_u64()).unwrap_or(0),
            )
        })
        .collect();
    let mut deduped = folds.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(
        folds.len(),
        deduped.len(),
        "duplicate folds detected (same (start,end) reported twice): folds={folds:?}"
    );

    // Plus the affirmative count: 1 trait + 1 impl + 2 method bodies +
    // 1 main fn body = 5 folds.
    assert_eq!(
        arr.len(),
        5,
        "expected exactly 5 folds; got {} — {arr:?}",
        arr.len()
    );

    client.shutdown();
}
