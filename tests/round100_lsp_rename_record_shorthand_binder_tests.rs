//! Round-100 BROKEN: `textDocument/rename` on a record SHORTHAND binder
//! (`let Point { x, y } = p`, cursor on `x`) corrupted the source.
//!
//! `collect_references_in_pattern` pushed `pattern.span` for a shorthand
//! binder, but that span is the record HEAD (the constructor name for a
//! nominal record, the opening `{` for an anon record) — not the field
//! token. The resulting edit rewrote the constructor / brace and left the
//! binder untouched: applying it produced `let XXX { x, y } = p`, which no
//! longer compiles.
//!
//! Fix: resolve the precise field-name offset within the pattern
//! (mirroring `ast_walk::check_shorthand_field_binder`) so the edit
//! targets the binder token. This test drives the live LSP server over
//! stdio, applies the returned edits, and asserts the constructor survives
//! and the binder + its use are renamed.
//!
//! Harness mirrors `tests/lsp_rename_binding_site_tests.rs`.

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
    // Apply each edit; sort so later positions are applied first to keep
    // earlier offsets valid. All edits here are single-line.
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

#[test]
fn rename_record_shorthand_binder_targets_field_not_constructor() {
    let mut client = LspClient::spawn();
    let uri = "file:///tmp/silt_r100_rn_shorthand.silt";
    // Line 0: `type Point { x: Int, y: Int }`
    // Line 1: `fn main() {`
    // Line 2: `  let Point { x, y } = Point { x: 1, y: 2 }`
    //                          ^ field `x` binder at char 14
    // Line 3: `  println("{x} {y}")`
    let text = "type Point { x: Int, y: Int }\n\
                fn main() {\n  \
                let Point { x, y } = Point { x: 1, y: 2 }\n  \
                println(\"{x} {y}\")\n}\n";
    client.did_open_and_wait(uri, text);

    let resp = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 14 },
            "newName": "renamed_x"
        }),
    );
    let result = resp.get("result").expect("rename has result");
    assert!(
        !result.is_null(),
        "rename on a shorthand binder must not return null; got {resp}"
    );
    let edits = result
        .pointer(&format!("/changes/{uri}"))
        .or_else(|| result.get("changes").and_then(|c| c.get(uri)))
        .and_then(|v| v.as_array())
        .expect("file edits");

    // The binder edit must target the FIELD token (line 2), NOT the
    // constructor `Point` (which starts at char 6). char 14 is the `x`.
    let binder_edit = edits
        .iter()
        .find(|e| e.pointer("/range/start/line").and_then(|v| v.as_u64()) == Some(2))
        .expect("an edit on the binder line");
    let start_char = binder_edit
        .pointer("/range/start/character")
        .and_then(|v| v.as_u64())
        .unwrap();
    assert!(
        start_char >= 12,
        "binder edit must target the field token (>=12), not the `Point` \
         constructor head (char 6); got char {start_char}"
    );

    // Applying every edit must keep `Point` intact and rename the binder
    // plus its use — the result must still be valid (compiling) code.
    let applied = apply_edits(text, edits);
    assert!(
        applied.contains("let Point { renamed_x, y } ="),
        "constructor `Point` must survive and the binder be renamed; got:\n{applied}"
    );
    assert!(
        applied.contains("{renamed_x}"),
        "the binder's use inside the interpolation must be renamed; got:\n{applied}"
    );
    assert!(
        !applied.contains("renamed_x { x, y }"),
        "the constructor name must NOT be clobbered; got:\n{applied}"
    );
    client.shutdown();
}
