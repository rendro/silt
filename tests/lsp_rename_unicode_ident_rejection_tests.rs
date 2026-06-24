//! Regression: LSP rename must reject new names that begin with a
//! non-ASCII Unicode letter, because the lexer's identifier-start set
//! is ASCII-only (`'a'..='z' | 'A'..='Z' | '_'` at `src/lexer.rs`).
//!
//! Before the fix, `is_valid_silt_ident` used `char::is_alphabetic` for
//! the first character, which returns `true` for Unicode letters like
//! `é` / `名` / `équipe`. rename then happily produced a `WorkspaceEdit`
//! rewriting every reference to a name that fails to lex on the next
//! `silt run` / `check`, silently corrupting the user's source.
//!
//! The fix switches the first-char check to `is_ascii_alphabetic`, so
//! the rename handler returns an `InvalidParams` error
//! (`is not a valid silt identifier`) instead of an edit.
//!
//! This locks the behaviour end-to-end through the LSP transport.
//! Harness mirrors `tests/lsp_rename_gated_constructor_rejection_tests.rs`.

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
                Err(RecvTimeoutError::Timeout) => {
                    panic!("timed out waiting for response id={id}");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("server disconnected waiting for id={id}");
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

/// Drive `textDocument/rename` on a user-defined `fn foo` with a
/// Unicode-leading new name and assert the server rejects it with an
/// `InvalidParams` error mentioning "not a valid silt identifier",
/// rather than returning a `WorkspaceEdit`.
fn assert_unicode_rename_rejected(new_name: &str) {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_unicode_ident.silt";
    // `foo` starts at line=0, char=3 (`fn foo`).
    let source = "fn foo() { 0 }\nfn main() { foo() }\n";
    client.did_open_and_wait(uri, source);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
            "newName": new_name
        }),
    );

    // The fix must reject the rename: an `error` response with the
    // identifier-validation message. A `WorkspaceEdit` (a `result` with
    // non-empty `changes`) would mean the server is about to corrupt the
    // user's source with a name the lexer cannot tokenize.
    let error_msg = resp
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let has_changes = resp
        .pointer("/result/changes")
        .and_then(|c| c.as_object())
        .map(|o| !o.is_empty())
        .unwrap_or(false);

    assert!(
        !has_changes,
        "rename to Unicode-leading name `{new_name}` must NOT return a \
         WorkspaceEdit — the resulting source fails to lex; got {resp}"
    );
    assert!(
        error_msg.contains("is not a valid silt identifier"),
        "rename to Unicode-leading name `{new_name}` must be rejected \
         with an InvalidParams `is not a valid silt identifier` error; \
         got {resp}"
    );
    client.shutdown();
}

#[test]
fn rename_to_latin_accented_name_is_rejected() {
    assert_unicode_rename_rejected("é");
}

#[test]
fn rename_to_cjk_name_is_rejected() {
    assert_unicode_rename_rejected("名");
}

#[test]
fn rename_to_accented_word_is_rejected() {
    assert_unicode_rename_rejected("équipe");
}

/// Positive control: a plain ASCII new name still succeeds, so the
/// fix only narrows the first-char set and does not block legitimate
/// renames.
#[test]
fn rename_to_ascii_name_still_succeeds() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_rn_ascii_ident.silt";
    let source = "fn foo() { 0 }\nfn main() { foo() }\n";
    client.did_open_and_wait(uri, source);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 3 },
            "newName": "bar"
        }),
    );
    let result = resp.get("result").expect("rename result present");
    assert!(
        !result.is_null(),
        "rename to plain ASCII name `bar` must succeed; got {resp}"
    );
    client.shutdown();
}
