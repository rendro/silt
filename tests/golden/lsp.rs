//! The golden harness's LSP client: start `silt lsp` in a case's
//! directory, open the entry file, and collect the diagnostics the
//! server publishes. Used by `-- cmd: lsp` cases and by the LSP door of
//! the verdict mode.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long the server gets to exit after `exit` before it is killed.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

/// One published diagnostic, its position converted to what the CLI
/// prints: a 1-based line and a 1-based column counted in characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LspDiagnostic {
    pub line: u64,
    pub col: u64,
    pub severity: String,
    pub message: String,
}

/// What one session with the server yielded.
pub struct LspSession {
    /// The diagnostics last published for each file, keyed by its path
    /// relative to the case directory with `/` separators. Every file
    /// the server published for is here, the entry file always.
    pub files: BTreeMap<String, Vec<LspDiagnostic>>,
    /// The server's exit status after `shutdown` and `exit`.
    pub code: Option<i32>,
    /// Everything the server wrote to stderr.
    pub stderr: String,
    /// Why the session failed, when it did: no publish for the entry file
    /// before the deadline, a protocol error, or a server that did not
    /// exit.
    pub error: Option<String>,
}

impl LspSession {
    /// The diagnostics as the `-- cmd: lsp` output: one line per
    /// diagnostic, `line:col severity message`, sorted. A diagnostic of a
    /// file other than `entry` is prefixed with that file's path and a
    /// colon. A message's line breaks are written as `\n`.
    pub fn render(&self, entry: &str) -> String {
        let mut out = String::new();
        let mut files: Vec<&String> = self.files.keys().collect();
        // The entry file first, then the others by path.
        files.sort_by_key(|f| (f.as_str() != entry, f.as_str()));
        for file in files {
            let mut diags = self.files[file].clone();
            diags.sort();
            for d in diags {
                if file != entry {
                    out.push_str(file);
                    out.push(':');
                }
                out.push_str(&format!(
                    "{}:{} {} {}\n",
                    d.line,
                    d.col,
                    d.severity,
                    d.message.replace('\n', "\\n")
                ));
            }
        }
        out
    }
}

/// A `file://` URI for `path`, which is absolute.
fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        // A Windows path, `C:/...`.
        uri.push('/');
    }
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// The path a `file://` URI names, with percent escapes decoded and `/`
/// separators; `None` for another scheme.
fn uri_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok());
        if bytes[i] == b'%'
            && let Some(Ok(b)) = hex.map(|h| u8::from_str_radix(h, 16))
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let mut path = String::from_utf8_lossy(&out).into_owned();
    // `/C:/...` names `C:/...`.
    if path.len() > 2 && path.as_bytes()[2] == b':' && path.starts_with('/') {
        path.remove(0);
    }
    Some(path)
}

/// `path` relative to one of `roots`, compared without regard to case
/// on Windows.
fn relative_to(path: &str, roots: &[String]) -> Option<String> {
    for root in roots {
        let root = root.trim_end_matches('/');
        let matches = if cfg!(windows) {
            path.len() > root.len()
                && path[..root.len()].eq_ignore_ascii_case(root)
                && path.as_bytes()[root.len()] == b'/'
        } else {
            path.len() > root.len() && path.starts_with(root) && path.as_bytes()[root.len()] == b'/'
        };
        if matches {
            return Some(path[root.len() + 1..].to_string());
        }
    }
    None
}

/// Convert a 0-based LSP position (UTF-16 code units) in `text` to the
/// 1-based line and character column the CLI prints.
fn cli_position(text: &str, line: u64, character: u64) -> (u64, u64) {
    let Some(line_text) = text.split('\n').nth(line as usize) else {
        return (line + 1, character + 1);
    };
    let mut units = 0u64;
    let mut chars = 0u64;
    for c in line_text.chars() {
        if units >= character {
            break;
        }
        units += c.len_utf16() as u64;
        chars += 1;
    }
    if units < character {
        // Past the end of the line: count the rest as characters.
        chars += character - units;
    }
    (line + 1, chars + 1)
}

fn severity_name(value: &Value) -> String {
    match value.as_u64() {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        Some(4) => "hint",
        _ => "none",
    }
    .to_string()
}

struct Client {
    stdin: ChildStdin,
    rx: Receiver<Value>,
    deadline: Instant,
    /// The latest `publishDiagnostics` params for each URI.
    published: BTreeMap<String, Value>,
}

impl Client {
    fn send(&mut self, msg: Value) -> Result<(), String> {
        let body = serde_json::to_string(&msg).expect("serialize");
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        self.stdin
            .write_all(framed.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("cannot write to silt lsp: {e}"))
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Read messages, recording every publish, until `stop` accepts one.
    fn read_until(&mut self, what: &str, stop: impl Fn(&Value) -> bool) -> Result<(), String> {
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            let msg = match self.rx.recv_timeout(left) {
                Ok(msg) => msg,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("timed out waiting for {what}"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!("silt lsp closed its output before {what}"));
                }
            };
            if msg["method"] == "textDocument/publishDiagnostics"
                && let Some(uri) = msg["params"]["uri"].as_str()
            {
                self.published
                    .insert(uri.to_string(), msg["params"].clone());
            }
            if stop(&msg) {
                return Ok(());
            }
        }
    }

    fn response(&mut self, id: u64, what: &str) -> Result<(), String> {
        self.read_until(what, |m| m["id"] == id && m.get("method").is_none())
    }
}

/// Run one session: start `silt lsp` in `dir` with `dir` as the
/// workspace root, open `entry` (relative to `dir`), wait for its
/// diagnostics, and shut the server down. Everything happens before
/// `timeout` runs out.
pub fn session(dir: &Path, entry: &str, timeout: Duration) -> LspSession {
    let root = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("lsp")
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn silt lsp");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut err_pipe = child.stderr.take().expect("stderr");
    let (tx, rx) = channel();
    std::thread::spawn(move || reader_loop(stdout, tx));
    let err_reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = err_pipe.read_to_end(&mut s);
        s
    });
    let mut client = Client {
        stdin,
        rx,
        deadline: Instant::now() + timeout,
        published: BTreeMap::new(),
    };

    let entry_path = root.join(entry);
    let entry_uri = file_uri(&entry_path);
    let text = std::fs::read(&entry_path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let unsaved = unsaved_buffers(&root);
    let result = drive(&mut client, &root, &entry_uri, text, &unsaved);

    // Close stdin so a server that ignored `exit` sees EOF, then give it
    // a moment before killing it.
    drop(client.stdin);
    let exit_deadline = Instant::now() + EXIT_TIMEOUT;
    let mut exited = None;
    while Instant::now() < exit_deadline {
        match child.try_wait() {
            Ok(Some(status)) => {
                exited = Some(status);
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut error = result.err();
    let code = match exited {
        Some(status) => status.code(),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            error.get_or_insert_with(|| "silt lsp did not exit after `exit`".to_string());
            None
        }
    };
    let stderr = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).into_owned();

    let roots = vec![
        root.to_string_lossy().replace('\\', "/"),
        dir.to_string_lossy().replace('\\', "/"),
    ];
    let mut files = BTreeMap::new();
    if !client.published.contains_key(&entry_uri) {
        error.get_or_insert_with(|| format!("no diagnostics were published for {entry}"));
    }
    for (uri, params) in &client.published {
        let rel = if uri == &entry_uri {
            entry.replace('\\', "/")
        } else {
            match uri_path(uri).and_then(|p| relative_to(&p, &roots)) {
                Some(rel) => rel,
                // A file outside the case: not part of what it shows.
                None => continue,
            }
        };
        let text = std::fs::read(root.join(&rel))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let diags = params["diagnostics"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|d| {
                let start = &d["range"]["start"];
                let (line, col) = cli_position(
                    &text,
                    start["line"].as_u64().unwrap_or(0),
                    start["character"].as_u64().unwrap_or(0),
                );
                LspDiagnostic {
                    line,
                    col,
                    severity: severity_name(&d["severity"]),
                    message: d["message"].as_str().unwrap_or_default().to_string(),
                }
            })
            .collect();
        files.insert(rel, diags);
    }
    LspSession {
        files,
        code,
        stderr,
        error,
    }
}

/// The editor buffers of a case: each file `<name>.unsaved` in `root`
/// (at any depth) is the unsaved text of the file `<name>`, as
/// (`file://` URI of `<name>`, text).
fn unsaved_buffers(root: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "unsaved")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                out.push((file_uri(&path.with_extension("")), text));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

/// The protocol exchange of [`session`]. The `unsaved` buffers are
/// opened before the entry file.
fn drive(
    client: &mut Client,
    root: &Path,
    entry_uri: &str,
    text: String,
    unsaved: &[(String, String)],
) -> Result<(), String> {
    let root_uri = file_uri(root);
    client.request(
        1,
        "initialize",
        json!({
            "processId": null,
            "rootUri": root_uri,
            "workspaceFolders": [{"uri": root_uri, "name": "case"}],
            "capabilities": {},
        }),
    )?;
    client.response(1, "the initialize response")?;
    client.notify("initialized", json!({}))?;
    // A round trip, so that whatever the server published while it
    // loaded the workspace is read before the file is opened.
    client.request(2, "workspace/symbol", json!({"query": ""}))?;
    client.response(2, "the workspace/symbol response")?;
    client.published.remove(entry_uri);
    for (uri, text) in unsaved {
        client.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri,
                "languageId": "silt",
                "version": 1,
                "text": text,
            }}),
        )?;
    }
    client.notify(
        "textDocument/didOpen",
        json!({"textDocument": {
            "uri": entry_uri,
            "languageId": "silt",
            "version": 1,
            "text": text,
        }}),
    )?;
    client.read_until("diagnostics for the opened file", |m| {
        m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == entry_uri
    })?;
    // Another round trip, for what the server publishes for other files
    // along with the opened one.
    client.request(3, "workspace/symbol", json!({"query": ""}))?;
    client.response(3, "the second workspace/symbol response")?;
    client.request(4, "shutdown", Value::Null)?;
    client.response(4, "the shutdown response")?;
    client.notify("exit", Value::Null)
}

/// Decode `Content-Length` frames from the server's stdout and forward
/// each message to `tx`. Ends at EOF or on a malformed frame.
fn reader_loop<R: Read>(stdout: R, tx: Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.trim().eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().ok();
            }
        }
        let Some(n) = content_length else { return };
        let mut body = vec![0u8; n];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(msg) = serde_json::from_slice(&body) else {
            return;
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}
