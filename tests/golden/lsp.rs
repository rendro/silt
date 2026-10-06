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
    /// What the server answered to each request of the case, as the
    /// lines `render` appends.
    pub answers: String,
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
        out.push_str(&self.answers);
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
    /// The response `read_until` stopped at last.
    last: Value,
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
                self.last = msg;
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
pub fn session(dir: &Path, entry: &str, timeout: Duration, requests: &[String]) -> LspSession {
    // The case directory as an editor names it, and as the server names
    // the files it publishes for: canonical (on Windows the long form of
    // a short 8.3 name such as `RUNNER~1`), without the `\\?\` prefix.
    let root = silt::source::canonical_path(dir);
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
        last: Value::Null,
    };

    let entry_path = root.join(entry);
    let entry_uri = file_uri(&entry_path);
    let text = std::fs::read(&entry_path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let unsaved = unsaved_buffers(&root);
    let roots = vec![
        root.to_string_lossy().replace('\\', "/"),
        dir.to_string_lossy().replace('\\', "/"),
    ];
    let asked = Asked {
        requests,
        root: &root,
        roots: &roots,
        entry,
    };
    let mut answers = String::new();
    let result = drive(
        &mut client,
        &root,
        &entry_uri,
        text,
        &unsaved,
        &asked,
        &mut answers,
    );

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
        answers,
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
    asked: &Asked,
    answers: &mut String,
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
    for (k, request) in asked.requests.iter().enumerate() {
        let id = 10 + k as u64;
        let (method, params) = asked.message(request, entry_uri, &text)?;
        client.request(id, method, params)?;
        client.response(id, &format!("the answer to `{request}`"))?;
        answers.push_str(&format!("> {request}\n"));
        answers.push_str(&asked.render(request, &client.last));
    }
    client.request(4, "shutdown", Value::Null)?;
    client.response(4, "the shutdown response")?;
    client.notify("exit", Value::Null)
}

/// The requests of a case (`-- lsp: <request>`), and what is needed to
/// write them and to print their answers.
struct Asked<'a> {
    requests: &'a [String],
    root: &'a Path,
    roots: &'a [String],
    entry: &'a str,
}

/// The 0-based LSP position (UTF-16 code units) of the 1-based line and
/// character column `at` (`L:C`) in `text`.
fn lsp_position(text: &str, at: &str) -> Result<Value, String> {
    let bad = || format!("bad position {at:?}: write LINE:COLUMN");
    let (line, col) = at.split_once(':').ok_or_else(bad)?;
    let line: usize = line.parse().map_err(|_| bad())?;
    let col: usize = col.parse().map_err(|_| bad())?;
    if line == 0 || col == 0 {
        return Err(bad());
    }
    let line_text = text.split('\n').nth(line - 1).unwrap_or("");
    let character: usize = line_text.chars().take(col - 1).map(char::len_utf16).sum();
    Ok(json!({"line": line - 1, "character": character}))
}

impl Asked<'_> {
    /// The method and the parameters of `request`, about the entry file
    /// (`text`, at `entry_uri`).
    fn message(
        &self,
        request: &str,
        entry_uri: &str,
        text: &str,
    ) -> Result<(&'static str, Value), String> {
        let mut words = request.split_whitespace();
        let what = words.next().unwrap_or("");
        let doc = json!({"uri": entry_uri});
        let mut at = || lsp_position(text, words.next().unwrap_or(""));
        Ok(match what {
            "references" => (
                "textDocument/references",
                json!({"textDocument": doc, "position": at()?,
                       "context": {"includeDeclaration": true}}),
            ),
            "highlight" => (
                "textDocument/documentHighlight",
                json!({"textDocument": doc, "position": at()?}),
            ),
            "prepare-rename" => (
                "textDocument/prepareRename",
                json!({"textDocument": doc, "position": at()?}),
            ),
            "hover" => (
                "textDocument/hover",
                json!({"textDocument": doc, "position": at()?}),
            ),
            "rename" => {
                let position = at()?;
                let name = words
                    .next()
                    .ok_or("`rename LINE:COLUMN NAME` needs the name")?;
                (
                    "textDocument/rename",
                    json!({"textDocument": doc, "position": position, "newName": name}),
                )
            }
            "symbols" => (
                "workspace/symbol",
                json!({"query": words.next().unwrap_or("")}),
            ),
            other => return Err(format!("unknown `-- lsp:` request {other:?}")),
        })
    }

    /// `uri` as the case names the file, and the file's text.
    fn file(&self, uri: &str) -> (String, String) {
        let rel = uri_path(uri)
            .and_then(|p| relative_to(&p, self.roots))
            .unwrap_or_else(|| uri.to_string());
        let text = std::fs::read(self.root.join(&rel))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        (rel, text)
    }

    /// `range` of `text` as `line:col-line:col`, as the CLI counts.
    fn range(text: &str, range: &Value) -> String {
        let end_of = |key: &str| {
            let at = &range[key];
            cli_position(
                text,
                at["line"].as_u64().unwrap_or(0),
                at["character"].as_u64().unwrap_or(0),
            )
        };
        let ((l1, c1), (l2, c2)) = (end_of("start"), end_of("end"));
        format!("{l1}:{c1}-{l2}:{c2}")
    }

    /// `file:line:col-line:col` of a location.
    fn location(&self, uri: &str, range: &Value) -> String {
        let (rel, text) = self.file(uri);
        format!("{rel}:{}", Self::range(&text, range))
    }

    /// The answer `response` to `request` as lines: one per location,
    /// edit or symbol, sorted; `(nothing)` for an empty or null result;
    /// `error: ...` for an error.
    fn render(&self, request: &str, response: &Value) -> String {
        if let Some(message) = response["error"]["message"].as_str() {
            return format!("error: {message}\n");
        }
        let result = &response["result"];
        let what = request.split_whitespace().next().unwrap_or("");
        let entry_text = || {
            std::fs::read(self.root.join(self.entry))
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        let mut lines: Vec<String> = match what {
            "references" => result
                .as_array()
                .map(|a| a.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|loc| self.location(loc["uri"].as_str().unwrap_or(""), &loc["range"]))
                .collect(),
            "highlight" => result
                .as_array()
                .map(|a| a.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|h| Self::range(&entry_text(), &h["range"]))
                .collect(),
            "prepare-rename" if result.is_object() => {
                vec![Self::range(&entry_text(), result)]
            }
            "rename" => result["changes"]
                .as_object()
                .into_iter()
                .flatten()
                .flat_map(|(uri, edits)| {
                    edits
                        .as_array()
                        .map(|a| a.as_slice())
                        .unwrap_or_default()
                        .iter()
                        .map(|edit| {
                            format!(
                                "{} => {}",
                                self.location(uri, &edit["range"]),
                                edit["newText"].as_str().unwrap_or("")
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect(),
            "symbols" => result
                .as_array()
                .map(|a| a.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|symbol| {
                    let location = &symbol["location"];
                    let container = match symbol["containerName"].as_str() {
                        Some(container) => format!(" in {container}"),
                        None => String::new(),
                    };
                    format!(
                        "{} {} {}{container}",
                        self.location(location["uri"].as_str().unwrap_or(""), &location["range"]),
                        symbol_kind(&symbol["kind"]),
                        symbol["name"].as_str().unwrap_or(""),
                    )
                })
                .collect(),
            // The text, line by line as it is.
            "hover" => {
                return match result["contents"]["value"].as_str() {
                    Some(text) => text.lines().map(|line| format!("{line}\n")).collect(),
                    None => "(nothing)\n".to_string(),
                };
            }
            _ => Vec::new(),
        };
        if lines.is_empty() {
            return "(nothing)\n".to_string();
        }
        lines.sort_by_key(|line| sort_key(line));
        lines.iter().map(|line| format!("{line}\n")).collect()
    }
}

/// `file:line:col...` sorted by file, then by position as numbers.
fn sort_key(line: &str) -> (String, u64, u64) {
    let mut parts = line.splitn(3, ':');
    let first = parts.next().unwrap_or("");
    // A highlight has no file: `line:col-...`.
    let (file, line_no) = match first.parse::<u64>() {
        Ok(n) => (String::new(), Some(n)),
        Err(_) => (first.to_string(), None),
    };
    let number = |text: Option<&str>| {
        text.map(|t| {
            t.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|digits| digits.parse::<u64>().ok())
        .unwrap_or(0)
    };
    match line_no {
        Some(n) => (file, n, number(parts.next())),
        None => (file, number(parts.next()), number(parts.next())),
    }
}

fn symbol_kind(kind: &Value) -> &'static str {
    match kind.as_u64() {
        Some(5) => "class",
        Some(10) => "enum",
        Some(11) => "interface",
        Some(12) => "function",
        Some(13) => "variable",
        Some(14) => "constant",
        Some(22) => "enum-member",
        Some(23) => "struct",
        Some(26) => "type-parameter",
        _ => "symbol",
    }
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
