//! Integration tests for round 62 phase-2 LSP doc extraction:
//! every `docs/stdlib/*.md` file's prose has been inlined into
//! `src/typechecker/builtins/docs.rs` as `*_MD` raw-string constants
//! and attached to the corresponding `env.bindings` entries via
//! `attach_module_docs` / `attach_module_overview` /
//! `attach_module_docs_filtered`. The LSP `Server` ingests these
//! through `typechecker::builtin_docs()` and surfaces them via
//! hover, completion, and signature-help.
//!
//! These tests spawn the compiled `silt lsp` binary as a subprocess
//! and exercise the LSP request handlers end-to-end. Helpers are
//! local to this file (kept identical to `tests/lsp.rs` so this
//! file is independently buildable).

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);
static URI_COUNTER: AtomicU64 = AtomicU64::new(1);

const READ_TIMEOUT: Duration = Duration::from_secs(10);

fn next_id() -> u64 {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn unique_uri() -> String {
    let n = URI_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("file:///tmp/silt_lsp_builtin_doc_test_{n}.silt")
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

    fn recv_response_for(&self, id: u64) -> ServerMessage {
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
                    panic!("silt lsp server closed its stdout unexpectedly");
                }
            }
        }
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
        let _ = self.recv_response_for(id);
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
        // Drain at least one publishDiagnostics so we know the
        // server has parsed the document.
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::from_millis(0));
            if remaining.is_zero() {
                return;
            }
            match self.rx.recv_timeout(remaining) {
                Ok(msg) => {
                    if msg.get("method").and_then(|v| v.as_str())
                        == Some("textDocument/publishDiagnostics")
                        && msg.pointer("/params/uri").and_then(|v| v.as_str()) == Some(uri)
                    {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }

    fn hover(&mut self, uri: &str, line: u32, character: u32) -> Value {
        let id = next_id();
        self.send_request(
            id,
            "textDocument/hover",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            }),
        );
        self.recv_response_for(id)
    }

    fn completion(&mut self, uri: &str, line: u32, character: u32) -> Value {
        let id = next_id();
        self.send_request(
            id,
            "textDocument/completion",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            }),
        );
        self.recv_response_for(id)
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
                Ok(None) => thread::sleep(Duration::from_millis(20)),
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

/// Extract the Markdown hover content text from a hover response.
/// Returns `None` if the response had no hover (server returned
/// `null`) or the contents shape is unexpected.
fn hover_markdown(resp: &Value) -> Option<String> {
    let contents = resp.pointer("/result/contents")?;
    // MarkupContent { kind: "markdown", value: "..." } shape.
    let value = contents.get("value")?.as_str()?;
    Some(value.to_string())
}

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn hover_on_list_map_returns_markdown_doc() {
    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    let source = "import list\n\nfn main() {\n    let xs = list.map([1, 2, 3], fn(x) { x + 1 })\n    xs\n}\n";
    client.did_open_and_wait(&uri, source);

    // Cursor on `map` in `list.map`. The line is
    // `    let xs = list.map([1, 2, 3], fn(x) { x + 1 })`
    // and `map` spans columns 18..21 (0-indexed). Use 19 to be
    // squarely inside.
    let resp = client.hover(&uri, 3, 19);
    let md = hover_markdown(&resp).expect("expected hover markdown for list.map");
    // The list.map section in list.md begins with the signature
    // line — accept any of the per-name body markers.
    assert!(
        md.contains("list.map") || md.contains("map") || md.contains("List"),
        "hover on list.map should surface its inlined doc; got:\n{md}"
    );
    // Look for prose unique to the list.map section.
    assert!(
        md.contains("Apply") || md.contains("transform") || md.contains("each"),
        "hover on list.map should mention the function's purpose; got:\n{md}"
    );
    client.shutdown();
}

#[test]
fn completion_for_list_module_includes_docs() {
    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    let source = "import list\n\nfn main() {\n    list.\n}\n";
    client.did_open_and_wait(&uri, source);

    // Cursor immediately after `list.` on line 3 (0-indexed).
    let resp = client.completion(&uri, 3, 9);
    let items = resp
        .pointer("/result/items")
        .or_else(|| resp.pointer("/result"))
        .and_then(|v| v.as_array())
        .expect("expected completion items array");
    assert!(!items.is_empty(), "list.<dot> should produce completions");

    // At least one of the function items must carry `documentation`
    // populated from the inlined builtin docs.
    let mut with_doc = 0usize;
    for it in items {
        if it.get("documentation").is_some() {
            with_doc += 1;
        }
    }
    assert!(
        with_doc > 0,
        "expected at least one list.* completion to carry documentation \
         (round 62 phase-2 builtin doc inlining); items: {items:?}"
    );
    client.shutdown();
}

#[test]
fn hover_on_math_cos_includes_signature_and_doc() {
    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    let source = "import math\n\nfn main() {\n    math.cos(0.0)\n}\n";
    client.did_open_and_wait(&uri, source);

    // Cursor on `cos` in `math.cos`, line 3 col 9-ish.
    let resp = client.hover(&uri, 3, 10);
    let md = hover_markdown(&resp).expect("expected hover markdown for math.cos");
    // The math.cos doc section's body starts with the signature
    // block and then the prose `Returns the cosine of \`x\``.
    assert!(
        md.contains("cosine"),
        "hover on math.cos should include the prose 'cosine'; got:\n{md}"
    );
    client.shutdown();
}

#[test]
fn hover_on_println_returns_globals_doc() {
    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    let source = "fn main() {\n    println(\"hi\")\n}\n";
    client.did_open_and_wait(&uri, source);

    // Cursor on `println` line 1 col 6.
    let resp = client.hover(&uri, 1, 6);
    let md = hover_markdown(&resp).expect("expected hover markdown for println");
    // The globals.md `## \`println\`` section talks about printing
    // a value followed by a newline.
    assert!(
        md.contains("newline") || md.contains("Display"),
        "hover on println should include the globals.md prose; got:\n{md}"
    );
    client.shutdown();
}

#[test]
fn hover_on_io_error_variant_returns_errors_doc() {
    let mut client = LspClient::spawn();
    client.initialize();
    let uri = unique_uri();
    let source = "import io\n\nfn main() {\n    let e = IoNotFound(\"x\")\n    e\n}\n";
    client.did_open_and_wait(&uri, source);

    // Cursor on `IoNotFound` line 3 col 14.
    let resp = client.hover(&uri, 3, 14);
    let md = hover_markdown(&resp).expect("expected hover markdown for IoNotFound");
    // The IoError section in errors.md mentions the variant table.
    assert!(
        md.contains("IoNotFound") || md.contains("path") || md.contains("Variant"),
        "hover on IoNotFound should include the IoError section; got:\n{md}"
    );
    client.shutdown();
}

/// Coverage smoke test (round 62 phase-2 lock). Every authoritative
/// qualified builtin name must have a non-empty doc string. Adding
/// a new builtin without a `## \`<name>\`` section in the matching
/// `*_MD` blob fails this test.
#[test]
fn every_authoritative_builtin_has_a_non_empty_doc_via_lsp_pipeline() {
    let docs = silt::typechecker::builtin_docs();
    let sigs = silt::typechecker::builtin_type_signatures();

    let mut missing: Vec<String> = Vec::new();
    for name in sigs.keys() {
        match docs.get(name) {
            None => missing.push(name.clone()),
            Some(d) if d.trim().is_empty() => missing.push(name.clone()),
            _ => {}
        }
    }
    if !missing.is_empty() {
        missing.sort();
        panic!(
            "{} authoritative builtin name(s) lack a non-empty doc \
             string: {:?}\n\nThe LSP `Server` populates its \
             `builtin_docs` cache from `typechecker::builtin_docs()` \
             at startup. Each entry feeds hover / completion / \
             signature-help. Adding a new builtin without inlining \
             its prose into the corresponding `super::docs::*_MD` \
             blob in `src/typechecker/builtins/docs.rs` (per round \
             62 phase-2) fails this test.",
            missing.len(),
            missing,
        );
    }
}
