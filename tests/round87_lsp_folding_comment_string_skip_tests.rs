//! Round-87 LATENT: `folding::compute_span_end_line` walked raw source
//! counting `{`/`}` to find the matching close-brace of a `type`,
//! `trait`, or `trait_impl` decl. It correctly skipped simple `"..."`
//! strings but did NOT skip:
//!
//!   * `--` line comments,
//!   * `{- ... -}` block comments (nestable),
//!   * `"""..."""` triple-quoted strings.
//!
//! A `}` inside any of those would decrement the depth counter too early
//! and collapse the fold range to a couple of lines — for the
//! `--`/block-comment cases the fold would end on the very line of the
//! stray `}`. The sibling `text_utils::match_closing_brace` already
//! handled all three kinds; round-87 extracts a shared scanner so the
//! folding-range path inherits the comment/string skip logic.
//!
//! These tests drive the real LSP `textDocument/foldingRange` handler
//! via subprocess (matching the existing folding-range subprocess
//! harness in `tests/round76_lsp_folding_no_dups_tests.rs` and
//! `tests/lsp_tier2_tests.rs`).

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

/// Fetch all folds for `src`, return them as a Vec of (startLine,
/// endLine) tuples.
fn folds_for(uri: &str, src: &str) -> Vec<(u64, u64)> {
    let mut client = LspClient::spawn();
    client.did_open_and_wait(uri, src);
    let resp = client.request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    let arr = resp
        .get("result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let out = arr
        .iter()
        .map(|f| {
            (
                f.get("startLine").and_then(|l| l.as_u64()).unwrap_or(0),
                f.get("endLine").and_then(|l| l.as_u64()).unwrap_or(0),
            )
        })
        .collect();
    client.shutdown();
    out
}

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn folding_type_body_with_line_comment_containing_close_brace() {
    // type body has a `-- comment }` line in the middle. The `}` inside
    // the line comment must NOT short-circuit brace matching — the fold
    // must end at the real closing `}` on line 3.
    //
    // Lines (0-indexed):
    //   0: type Foo {
    //   1:   -- the close } here
    //   2:   bar: Int,
    //   3: }
    let src = "type Foo {\n  -- the close } here\n  bar: Int,\n}\n";
    let folds = folds_for("file:///tmp/silt_r87_fold_line_comment.silt", src);
    // Exactly one fold (the type decl) and it spans 0..3.
    assert_eq!(
        folds.len(),
        1,
        "expected exactly 1 fold for the type decl; got {folds:?}"
    );
    assert_eq!(
        folds[0],
        (0, 3),
        "fold should span the full type body (line 0 to line 3), not \
         stop at the `}}` inside the `--` line comment; got {folds:?}"
    );
}

#[test]
fn folding_type_body_with_block_comment_containing_close_brace() {
    // type body has a `{- comment } -}` line in the middle. The `}`
    // inside the block comment (and the `{-` open) must NOT corrupt
    // the brace count. Fold must end at the real `}` on line 3.
    //
    // Lines (0-indexed):
    //   0: type Foo {
    //   1:   {- the close } here -}
    //   2:   bar: Int,
    //   3: }
    let src = "type Foo {\n  {- the close } here -}\n  bar: Int,\n}\n";
    let folds = folds_for("file:///tmp/silt_r87_fold_block_comment.silt", src);
    assert_eq!(
        folds.len(),
        1,
        "expected exactly 1 fold for the type decl; got {folds:?}"
    );
    assert_eq!(
        folds[0],
        (0, 3),
        "fold should span the full type body (line 0 to line 3), not \
         stop at the `}}` inside the `{{- ... -}}` block comment; \
         got {folds:?}"
    );
}

#[test]
fn folding_trait_impl_with_triple_string_containing_close_brace() {
    // Triple-quoted strings can carry both `"` and `}` in their body,
    // and the closing `"""` is the only delimiter. The pre-fix scanner
    // only knew how to skip simple `"..."` strings, so a triple string
    // with an embedded `"` would split the body into mis-paired skip
    // windows and leak a `}` into the brace counter.
    //
    // We use a `trait Greet for Foo { ... }` impl because `type` bodies
    // are records/enums and don't accept string-literal field defaults.
    //
    // Lines (0-indexed):
    //   0: type Foo { val: Int }
    //   1: trait Greet for Foo {
    //   2:   fn greet(self) -> String {
    //   3:     """a"b}c"""
    //   4:   }
    //   5: }
    //
    // The triple string `"""a"b}c"""` contains a `"` (between `a` and
    // `b`) and a `}` (after `b`). Pre-fix: the simple-string skip
    // mode pairs the `"` chars unevenly and the inner `}` decrements
    // depth — collapsing the trait-impl fold's end_line.
    let src = "\
type Foo { val: Int }
trait Greet for Foo {
  fn greet(self) -> String {
    \"\"\"a\"b}c\"\"\"
  }
}
";
    let folds = folds_for("file:///tmp/silt_r87_fold_triple_string.silt", src);

    // The fold we care about is the trait-impl span (starts at line 1).
    // It must end on line 5 (the real `}` of the trait impl), not
    // earlier due to the embedded `}` inside the triple-quoted string.
    let impl_fold = folds.iter().find(|(s, _)| *s == 1).copied();
    let Some((start, end)) = impl_fold else {
        panic!(
            "no fold found starting at line 1 (the `trait Greet for Foo` header); got folds={folds:?}"
        );
    };
    assert_eq!(start, 1, "impl fold should start at line 1");
    assert_eq!(
        end, 5,
        "impl fold should end at line 5 (the trait-impl's real \
         closing `}}`), not at a `}}` inside the `\"\"\"...\"\"\"` \
         triple-quoted string; got folds={folds:?}"
    );
}
