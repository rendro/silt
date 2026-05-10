//! Round 81 (DX-G4): `textDocument/completion`'s dot-completion path
//! must narrow the surfaced method set to those valid for the
//! receiver's inferred type.
//!
//! Pre-round-81 behaviour: the dot-completion path emitted record
//! fields when applicable but never surfaced trait methods. As a
//! result, `xs.|` on a `let xs: List(Int)` produced an empty
//! completion list — silt was a "second source of truth" relative to
//! the typechecker because the LSP couldn't tell the user which
//! methods were dispatchable on a List.
//!
//! Round 81 ships a minimum-viable type-aware narrowing pass:
//!
//!   - When the receiver is a let-bound local with a recoverable type
//!     (annotation or initializer-inferred), the LSP filters the
//!     emitted method set to methods declared for that type's
//!     canonical head name.
//!   - When the receiver type cannot be inferred narrowly (no
//!     annotation, complex expression, generic stuck on type
//!     variables), the LSP falls back to the union of every method
//!     name declared in the program — discoverable but unfiltered.
//!
//! The minimum-viable contract: filtering NEVER hides completions on a
//! receiver whose type the LSP can't pin down. Out of scope for this
//! round: chained calls past the first dot, generic instantiation,
//! trait constraints. See `src/lsp/completion.rs::dot_completions`
//! and the `methods_for_receiver` helper for the narrowing logic.
//!
//! All tests drive a real `silt lsp` subprocess end-to-end over the
//! LSP JSON-RPC transport so the full pipeline (lex → parse →
//! typecheck → publish → completion) is exercised.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
static URI_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generous enough for a debug-build cold start on slow CI but short
/// enough that a broken server fails the test promptly.
const READ_TIMEOUT: Duration = Duration::from_secs(15);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri(tag: &str) -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_round81_{tag}_{n}.silt")
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
            .expect("spawn silt lsp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = channel::<ServerMessage>();
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

/// Pull `(label, kind)` pairs out of either the bare-array or
/// `CompletionList` response shapes. Mirrors the helper in
/// `tests/lsp_anon_record_field_completion_tests.rs`.
fn extract_completion_labels(result: &Value) -> Vec<String> {
    let arr = if let Some(arr) = result.as_array() {
        arr.clone()
    } else if let Some(arr) = result.pointer("/items").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|it| {
            it.get("label")
                .and_then(|l| l.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

// ── (a) Typed List local — narrow to List methods ─────────────────────
//
// Receiver is `let xs: List(Int) = [1, 2, 3]`. The LSP must surface
// methods that exist on a `List(Int)` receiver — at minimum the
// auto-derived built-in trait methods (`display`, `equal`, `compare`,
// `hash`) — and must NOT surface methods that only exist on `String`
// receivers (e.g. user-declared trait methods on String, or — should
// the LSP ever start surfacing module-style helpers — `to_upper`,
// `chars`, `split`).
//
// We assert positive presence of `display` (List auto-derives Display
// per `register_builtin_trait_impls` policy) and absence of
// String-specific user trait method names defined in the source. Names
// like `to_upper` / `chars` / `split` are stdlib module functions
// (`string.to_upper(s)`), not methods on a String value, so a silt
// program never reaches the LSP with them in the AST — to make the
// "must NOT include obviously-wrong methods" assertion meaningful we
// declare a `trait UpperCaser for String { fn to_upper(self) ... }`
// in the source and assert that `to_upper` does NOT appear in the
// completion for the `List` receiver. Without narrowing, both
// `display` (from List's auto-derive) AND `to_upper` (from the user
// String impl) would appear in `xs.|`. With narrowing, only List
// methods survive.

#[test]
fn dot_completion_on_typed_list_local_filters_to_list_methods() {
    let mut client = LspClient::spawn();
    let uri = unique_uri("typed_list_narrow");
    // Declare a String-only trait impl in the source so the program
    // has at least one method name that is incontestably wrong for a
    // List receiver. Without narrowing, `to_upper` would be in the
    // unfiltered union; the narrowed set must drop it.
    let text = "trait UpperCaser { fn to_upper(self) -> String }\n\
                trait UpperCaser for String { fn to_upper(self) -> String { \"X\" } }\n\
                fn main() {\n\
                \x20\x20let xs: List(Int) = [1, 2, 3]\n\
                \x20\x20let _ = xs.\n\
                }\n";
    client.did_open_and_wait(&uri, text);

    // Cursor placement: line 4 (0-indexed), column 13 — right after
    // the `.` in `xs.`. Counting:
    //   "  let _ = xs."
    //    0         1
    //    01234567890123
    //                 ^ char=13 sits one past the dot.
    let resp = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 4, "character": 13 },
            "context": { "triggerKind": 2, "triggerCharacter": "." }
        }),
    );
    let result = resp.get("result").expect("completion has result");
    let labels = extract_completion_labels(result);

    // Positive: List auto-derives Display, so `display` MUST appear in
    // the narrowed completion set.
    assert!(
        labels.contains(&"display".to_string()),
        "List receiver completion must include `display` (auto-derived); \
         got labels: {labels:?}"
    );
    // Negative: a String-only user trait method must NOT appear.
    // Pre-round-81 (no narrowing), it would.
    assert!(
        !labels.contains(&"to_upper".to_string()),
        "List receiver completion must NOT include String-only method \
         `to_upper`; got labels: {labels:?}"
    );

    client.shutdown();
}

// ── (b) Typed String local — narrow to String methods ────────────────
//
// Mirror of (a). Receiver is `let s: String = "hello"`. The LSP must
// surface `display` (String auto-derives Display) AND the
// String-specific user trait method `to_upper` (declared in the
// source); it must NOT surface a List-only user trait method like
// `head_int` from the same source.

#[test]
fn dot_completion_on_typed_string_local_filters_to_string_methods() {
    let mut client = LspClient::spawn();
    let uri = unique_uri("typed_string_narrow");
    let text = "trait UpperCaser { fn to_upper(self) -> String }\n\
                trait UpperCaser for String { fn to_upper(self) -> String { \"X\" } }\n\
                trait IntListHead { fn head_int(self) -> Int }\n\
                trait IntListHead for List { fn head_int(self) -> Int { 0 } }\n\
                fn main() {\n\
                \x20\x20let s: String = \"hello\"\n\
                \x20\x20let _ = s.\n\
                }\n";
    client.did_open_and_wait(&uri, text);

    // Cursor: line 6, char=12 — after `let _ = s.` (10 chars before
    // the `.` plus the `.` itself plus the leading two spaces gives 12).
    //   "  let _ = s."
    //    0         1
    //    012345678901
    //                ^ char=12
    let resp = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 6, "character": 12 },
            "context": { "triggerKind": 2, "triggerCharacter": "." }
        }),
    );
    let result = resp.get("result").expect("completion has result");
    let labels = extract_completion_labels(result);

    // Positive: `display` (auto-derived for String) AND `to_upper`
    // (user trait impl on String) must both appear.
    assert!(
        labels.contains(&"display".to_string()),
        "String receiver completion must include `display` (auto-derived); \
         got labels: {labels:?}"
    );
    assert!(
        labels.contains(&"to_upper".to_string()),
        "String receiver completion must include `to_upper` (user trait impl \
         on String); got labels: {labels:?}"
    );
    // Negative: a List-only user trait method must NOT appear.
    assert!(
        !labels.contains(&"head_int".to_string()),
        "String receiver completion must NOT include List-only method \
         `head_int`; got labels: {labels:?}"
    );

    client.shutdown();
}

// ── (c) Untyped local with unknowable type — full set fallback ───────
//
// Receiver is bound to a complex expression whose inferred type is a
// stuck type variable (no annotation, no concrete initializer). The
// minimum-viable contract: when narrowing isn't safe, the LSP must
// fall back to the union of every method name declared in the program
// — `to_upper` AND `head_int` from the example source must both
// appear, because the LSP cannot prove either one wrong.
//
// Test name documents the limit: `untyped_local` here means a binding
// whose recoverable type is `None` from the LSP's vantage point.
// (Round 81 minimum-viable: any receiver expression that is not a
// straight identifier resolving to an inferred non-`_` type lands in
// the fallback set. Future rounds can tighten this for literals and
// chained calls.)

#[test]
fn dot_completion_on_untyped_local_returns_full_set() {
    let mut client = LspClient::spawn();
    let uri = unique_uri("untyped_fallback");
    // The receiver `x` is bound to a recursive call that the LSP
    // cannot resolve to a concrete type — its `expr.ty` either stays
    // an unresolved variable or is missing entirely after typecheck,
    // depending on inference order. Either way the dot-completion
    // path's `find_ident_type_by_name` returns `None` (it explicitly
    // filters out types containing unresolved vars; see
    // `src/lsp/ast_walk.rs::has_unresolved_vars`), so the receiver
    // type stays `None` and the fallback union must surface both the
    // String-only `to_upper` and the List-only `head_int`.
    let text = "trait UpperCaser { fn to_upper(self) -> String }\n\
                trait UpperCaser for String { fn to_upper(self) -> String { \"X\" } }\n\
                trait IntListHead { fn head_int(self) -> Int }\n\
                trait IntListHead for List { fn head_int(self) -> Int { 0 } }\n\
                fn unknown(a) { unknown(a) }\n\
                fn main() {\n\
                \x20\x20let x = unknown(0)\n\
                \x20\x20let _ = x.\n\
                }\n";
    client.did_open_and_wait(&uri, text);

    // Cursor: line 7, char=12.
    //   "  let _ = x."
    //    0         1
    //    012345678901
    //                ^ char=12
    let resp = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 7, "character": 12 },
            "context": { "triggerKind": 2, "triggerCharacter": "." }
        }),
    );
    let result = resp.get("result").expect("completion has result");
    let labels = extract_completion_labels(result);

    // Both user-declared trait methods must appear in the fallback set
    // — narrowing only ever reduces noise; on an unknown receiver it
    // must NOT silently drop completions.
    assert!(
        labels.contains(&"to_upper".to_string()),
        "untyped receiver completion must include `to_upper` from the \
         fallback union (no narrowing took effect); got labels: {labels:?}"
    );
    assert!(
        labels.contains(&"head_int".to_string()),
        "untyped receiver completion must include `head_int` from the \
         fallback union (no narrowing took effect); got labels: {labels:?}"
    );

    client.shutdown();
}
