//! Regression tests for four tooling defects.
//!
//! * The language server died on a document that contains a non-ASCII
//!   character which the lexer rejects (a pasted typographic quotation
//!   mark was enough): it sliced the text inside that character. It
//!   also died on a request for a position behind the last line, and
//!   every other panic in a handler ended the session too.
//! * `silt test` reported PASS for a test that returns `Err(..)`.
//! * `silt check`, `silt run` and `silt test` decided "is there a
//!   `main`", "is this a test file" and "which tests match the filter"
//!   by scanning source lines, and disagreed with the parser and with
//!   each other.
//! * `silt test` dropped the type errors of imported modules, the
//!   compiler's warnings, and the static checks against a declared
//!   dependency, all of which `silt check` reports for the same file.
//!
//! Every test runs the built `silt` binary on files in a fresh temporary
//! directory, or talks to `silt lsp` over its standard input and
//! output, and asserts on exit status and output. Every process has a
//! timeout, so a hang fails the test instead of hanging the suite.
//!
//! Unless a test says that it is a guard, it fails without the fixes.
//! A guard pins behaviour that the fixes must not change.
//!
//! The single-command `silt check` / `run` / `test` cases are golden
//! cases `tests/golden/lang/tooling/wave1_tooling__*`. What stays here
//! talks to `silt lsp`, runs `silt test` without a file argument, or
//! compares the output of two commands.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Upper bound for one run of the binary, and for one answer of the
/// language server. What exceeds it is killed and reported as a hang.
const TIMEOUT: Duration = Duration::from_secs(20);

fn fresh_dir(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("silt_wave1_tooling_{pid}_{unique}_{label}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn read_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.replace("\r\n", "\n")
}

// ── Running the command line ─────────────────────────────────────────

// The fields are shown in failure messages through `Debug`.
#[allow(dead_code)]
#[derive(Debug)]
struct Outcome {
    /// Exit status; `None` if the process was ended by a signal.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// True if the run exceeded `TIMEOUT` and was killed.
    timed_out: bool,
}

impl Outcome {
    /// The header lines of the diagnostics on stderr
    /// (`error[type]: ...`, `warning[compile]: ...`), in order.
    fn diagnostics(&self) -> Vec<String> {
        self.stderr
            .lines()
            .map(str::trim_start)
            .filter(|line| line.starts_with("error[") || line.starts_with("warning["))
            .map(str::to_string)
            .collect()
    }
}

/// Source files in a fresh temporary directory. Removed on drop.
struct Project {
    dir: PathBuf,
    runs: u32,
}

impl Project {
    /// `files` are (path relative to the project, content) pairs.
    fn new(label: &str, files: &[(&str, &str)]) -> Project {
        let dir = fresh_dir(label);
        std::fs::create_dir_all(dir.join("work")).expect("create work dir");
        let project = Project { dir, runs: 0 };
        for (name, content) in files {
            let path = project.work().join(name);
            let parent = path.parent().expect("a file path has a parent");
            std::fs::create_dir_all(parent).expect("create source dir");
            std::fs::write(&path, content).expect("write source file");
        }
        project
    }

    fn work(&self) -> PathBuf {
        self.dir.join("work")
    }

    /// Run `silt <args>` once, with the project as working directory.
    ///
    /// Output goes to files outside the project rather than to pipes, so
    /// a child that is killed on timeout cannot leave the test blocked
    /// on a read.
    fn silt(&mut self, args: &[&str]) -> Outcome {
        self.silt_in("", args)
    }

    /// Like [`Project::silt`], with the working directory `subdir` of
    /// the project.
    fn silt_in(&mut self, subdir: &str, args: &[&str]) -> Outcome {
        self.runs += 1;
        let out_path = self.dir.join(format!("stdout_{}.txt", self.runs));
        let err_path = self.dir.join(format!("stderr_{}.txt", self.runs));
        let out_file = std::fs::File::create(&out_path).expect("create stdout file");
        let err_file = std::fs::File::create(&err_path).expect("create stderr file");

        let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .args(args)
            .current_dir(self.work().join(subdir))
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(out_file))
            .stderr(Stdio::from(err_file))
            .spawn()
            .expect("spawn silt");

        let started = Instant::now();
        let mut timed_out = false;
        let status = loop {
            match child.try_wait().expect("try_wait") {
                Some(status) => break status,
                None if started.elapsed() >= TIMEOUT => {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().expect("wait after kill");
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };

        let outcome = Outcome {
            code: status.code(),
            stdout: read_text(&out_path),
            stderr: read_text(&err_path),
            timed_out,
        };
        assert!(
            !outcome.timed_out,
            "`silt {}` hung and was killed after {TIMEOUT:?}\n{outcome:#?}",
            args.join(" ")
        );
        outcome
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ── Talking to the language server ───────────────────────────────────

fn reader_loop(stdout: std::process::ChildStdout, tx: std::sync::mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut header = String::new();
        let mut content_length: Option<usize> = None;
        loop {
            header.clear();
            match reader.read_line(&mut header) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
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

/// A `silt lsp` process, started by this test and ended by this test:
/// on drop it kills exactly the process it spawned, through its handle.
struct Lsp {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Value>,
    dir: PathBuf,
    next_id: u64,
}

impl Lsp {
    /// Start the server and complete the `initialize` handshake.
    fn start(label: &str) -> Lsp {
        let dir = fresh_dir(label);
        let err_file = std::fs::File::create(dir.join("stderr.txt")).expect("create stderr file");
        let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("lsp")
            .current_dir(&dir)
            .env("NO_COLOR", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(err_file))
            .spawn()
            .expect("spawn silt lsp");
        let stdin = child.stdin.take().expect("stdin of the server");
        let stdout = child.stdout.take().expect("stdout of the server");
        let (tx, rx) = channel::<Value>();
        std::thread::spawn(move || reader_loop(stdout, tx));
        let mut lsp = Lsp {
            child,
            stdin: Some(stdin),
            rx,
            dir,
            next_id: 0,
        };
        let answer = lsp.request("initialize", json!({ "capabilities": {} }));
        assert!(
            answer.get("result").is_some(),
            "the server must answer `initialize`: {answer}"
        );
        lsp.notify("initialized", json!({}));
        lsp
    }

    /// What the server has written to its stderr so far.
    fn stderr(&self) -> String {
        read_text(&self.dir.join("stderr.txt"))
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Why a test gave up on the server, with everything that helps to
    /// see what happened to it.
    fn gave_up(&mut self, what: &str) -> String {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => format!("the server has exited, {status}"),
            Ok(None) => "the server is still running".to_string(),
            Err(e) => format!("the state of the server is unknown: {e}"),
        };
        format!(
            "{what}; {status}\n--- stderr of the server ---\n{}",
            self.stderr()
        )
    }

    fn send(&mut self, msg: &Value) {
        let body = serde_json::to_string(msg).expect("serialize message");
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let stdin = self.stdin.as_mut().expect("stdin of the server is open");
        let written = stdin
            .write_all(framed.as_bytes())
            .and_then(|()| stdin.flush());
        if let Err(e) = written {
            let why = self.gave_up(&format!("cannot write to the server ({e})"));
            panic!("{why}");
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// The next message from the server that `wanted` accepts. Messages
    /// before it are dropped.
    fn wait_for(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(remaining) {
                Ok(msg) if wanted(&msg) => return msg,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    let why = self.gave_up(&format!("no {what} within {TIMEOUT:?}"));
                    panic!("{why}");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // Give the process a moment to be reaped, so that
                    // the report can name its exit status.
                    std::thread::sleep(Duration::from_millis(200));
                    let why = self.gave_up(&format!(
                        "the server closed its output while the test waited for {what}"
                    ));
                    panic!("{why}");
                }
            }
        }
    }

    /// Send a request and return the response to it, be it a result or
    /// an error.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        self.wait_for(&format!("response to `{method}`"), |msg| {
            msg.get("method").is_none() && msg.get("id").and_then(Value::as_u64) == Some(id)
        })
    }

    /// The diagnostics of the next `publishDiagnostics` for `uri`.
    fn next_diagnostics(&mut self, uri: &str) -> Vec<Value> {
        let msg = self.wait_for(&format!("diagnostics for {uri}"), |msg| {
            msg.get("id").is_none()
                && msg.get("method").and_then(Value::as_str)
                    == Some("textDocument/publishDiagnostics")
                && msg.pointer("/params/uri").and_then(Value::as_str) == Some(uri)
        });
        msg.pointer("/params/diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .expect("publishDiagnostics carries a list")
    }

    /// `didOpen`, and the diagnostics the server publishes for it.
    fn open(&mut self, uri: &str, text: &str) -> Vec<Value> {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": { "uri": uri, "languageId": "silt", "version": 1, "text": text }
            }),
        );
        self.next_diagnostics(uri)
    }

    /// `didChange` to the full text `text`, and the diagnostics the
    /// server publishes for it.
    fn change(&mut self, uri: &str, version: u32, text: &str) -> Vec<Value> {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }),
        );
        self.next_diagnostics(uri)
    }

    fn hover(&mut self, uri: &str, line: u32, character: u32) -> Value {
        self.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
        )
    }

    /// The server must be running, and must answer a hover on a name of
    /// a valid document with its type.
    fn assert_still_serving(&mut self, uri: &str, context: &str) {
        assert!(
            self.is_alive(),
            "{}",
            self.gave_up(&format!("{context}: the server must still be running"))
        );
        let diagnostics = self.change(uri, 1000, "fn answer() -> Int { 42 }\n");
        assert!(
            diagnostics.is_empty(),
            "{context}: a valid document has no diagnostics, got {diagnostics:?}"
        );
        let answer = self.hover(uri, 0, 4);
        let shown = answer
            .pointer("/result/contents/value")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            shown.contains("Int"),
            "{context}: hover on `answer` must show its type, got {answer}"
        );
    }

    /// `shutdown`, `exit`, and the exit status of the server.
    fn shut_down(mut self) -> Option<i32> {
        let answer = self.request("shutdown", Value::Null);
        assert!(
            answer.get("error").is_none(),
            "the server must accept `shutdown`: {answer}"
        );
        self.notify("exit", Value::Null);
        // Close our end of its input as well.
        self.stdin = None;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.code(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let why = self.gave_up("the server did not exit after `exit`");
                    panic!("{why}");
                }
            }
        }
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        // Ends the one process this value spawned, if it still runs.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The position of the first `needle` in `text`, as the language server
/// protocol counts: 0-based line, and UTF-16 code units from the start
/// of that line.
fn position_of(text: &str, needle: char) -> (u32, u32) {
    let offset = text.find(needle).expect("the text contains the character");
    let before = &text[..offset];
    let line = before.matches('\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let character = before[line_start..].encode_utf16().count() as u32;
    (line, character)
}

/// Open a valid document, change it to `text`, in which the lexer
/// rejects the character `rejected`, and ask for a hover.
///
/// The server must publish one error whose range is that character,
/// must answer the hover, and must go on serving.
fn assert_survives_rejected_character(label: &str, text: &str, rejected: char) {
    let mut lsp = Lsp::start(label);
    let uri = format!("file:///wave1_tooling/{label}.silt");

    let diagnostics = lsp.open(&uri, "fn main() { 1 }\n");
    assert!(
        diagnostics.is_empty(),
        "{label}: a valid document has no diagnostics, got {diagnostics:?}"
    );

    let diagnostics = lsp.change(&uri, 2, text);
    let (line, character) = position_of(text, rejected);
    let width = rejected.len_utf16() as u32;
    assert_eq!(
        diagnostics.len(),
        1,
        "{label}: expected the one error of the lexer, got {diagnostics:?}"
    );
    let diagnostic = &diagnostics[0];
    assert_eq!(
        diagnostic["severity"],
        json!(1),
        "{label}: the diagnostic must be an error: {diagnostic}"
    );
    assert_eq!(
        diagnostic["range"],
        json!({
            "start": { "line": line, "character": character },
            "end": { "line": line, "character": character + width }
        }),
        "{label}: the range must cover the rejected character {rejected:?}: {diagnostic}"
    );

    let answer = lsp.hover(&uri, 0, 4);
    assert!(
        answer.get("error").is_none(),
        "{label}: the hover must be answered with a result, got {answer}"
    );

    lsp.assert_still_serving(&uri, label);
    assert_eq!(
        lsp.shut_down(),
        Some(0),
        "{label}: the server must exit with status 0 after `shutdown` and `exit`"
    );
}

// ── The language server and non-ASCII characters ─────────────────────

/// The reported case: a typographic quotation mark, as pasted from a
/// word processor.
#[test]
fn lsp_survives_a_typographic_quotation_mark() {
    assert_survives_rejected_character("smart_quote", "fn main() {\n  println(“hello”)\n}\n", '“');
}

/// Characters of two, three and four bytes, where an identifier or an
/// expression is expected.
#[test]
fn lsp_survives_rejected_characters_of_every_width() {
    assert_survives_rejected_character("lambda", "fn main() {\n  let f = λx\n}\n", 'λ');
    assert_survives_rejected_character("euro", "fn main() {\n  let x = €\n}\n", '€');
    assert_survives_rejected_character("emoji", "fn main() {\n  let x = 😀\n}\n", '😀');
}

/// A no-break space looks like a space and is rejected like any other
/// character the lexer does not know.
#[test]
fn lsp_survives_a_no_break_space() {
    assert_survives_rejected_character(
        "no_break_space",
        "fn main() {\n  let x =\u{a0}1\n}\n",
        '\u{a0}',
    );
}

/// The rejected character inside a string interpolation, and after the
/// dot of a field access.
#[test]
fn lsp_survives_rejected_characters_inside_expressions() {
    assert_survives_rejected_character(
        "interpolation",
        "fn main() {\n  println(\"{é}\")\n}\n",
        'é',
    );
    assert_survives_rejected_character("field_access", "fn main() {\n  let t = 1\n  t.é\n}\n", 'é');
}

/// The rejected character as the first and as the last character of the
/// document, the latter without a line break behind it.
#[test]
fn lsp_survives_rejected_characters_at_the_ends_of_the_document() {
    assert_survives_rejected_character("first_character", "日本\n", '日');
    assert_survives_rejected_character("last_character", "fn main() { 1 }\n“", '“');
}

/// The rejected character behind text that shifts byte offsets, UTF-16
/// columns and code point columns apart: characters outside the basic
/// multilingual plane, an accented letter, CRLF line ends, and a
/// byte order mark.
#[test]
fn lsp_survives_rejected_characters_behind_other_non_ascii_text() {
    assert_survives_rejected_character(
        "behind_emoji",
        "fn main() {\n  let s = \"😀😀\" €\n}\n",
        '€',
    );
    assert_survives_rejected_character(
        "behind_crlf",
        "fn main() {\r\n  let s = \"café\" §\r\n}\r\n",
        '§',
    );
    assert_survives_rejected_character(
        "behind_byte_order_mark",
        "\u{feff}fn main() {\n  §\n}\n",
        '§',
    );
}

/// Guard: a valid document with non-ASCII text in a string and in a
/// comment is analysed as before.
#[test]
fn lsp_guard_valid_non_ascii_text_has_no_diagnostics() {
    let mut lsp = Lsp::start("valid_non_ascii");
    let uri = "file:///wave1_tooling/valid_non_ascii.silt";
    let text = "-- “quoted” 😀\nfn main() {\n  let cafe = \"“café” 😀\"\n  cafe\n}\n";
    let diagnostics = lsp.open(uri, text);
    assert!(
        diagnostics.is_empty(),
        "a valid document has no diagnostics, got {diagnostics:?}"
    );
    let answer = lsp.hover(uri, 1, 4);
    let shown = answer
        .pointer("/result/contents/value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        shown.contains("String"),
        "hover on `main` must show its type, got {answer}"
    );
    assert_eq!(lsp.shut_down(), Some(0));
}

/// Guard: an ASCII character that the lexer rejects gets a range of one
/// column, as before.
#[test]
fn lsp_guard_rejected_ascii_character_keeps_its_range() {
    assert_survives_rejected_character("ascii", "fn main() {\n  let s = @\n}\n", '@');
}

// ── The language server and positions behind the document ────────────

/// A request for a position behind the last line of a document that
/// does not end with a line break. The offset computed for it was one
/// byte past the end of the text, and signature help sliced with it.
#[test]
fn lsp_answers_requests_for_a_position_behind_the_last_line() {
    let mut lsp = Lsp::start("behind_last_line");
    let uri = "file:///wave1_tooling/behind_last_line.silt";
    let diagnostics = lsp.open(uri, "fn main() { 1 }");
    assert!(
        diagnostics.is_empty(),
        "a valid document has no diagnostics, got {diagnostics:?}"
    );

    let position = json!({
        "textDocument": { "uri": uri },
        "position": { "line": 5, "character": 0 }
    });
    for method in [
        "textDocument/signatureHelp",
        "textDocument/hover",
        "textDocument/completion",
        "textDocument/definition",
        "textDocument/documentHighlight",
        "textDocument/prepareRename",
    ] {
        let answer = lsp.request(method, position.clone());
        assert!(
            answer.get("error").is_none(),
            "`{method}` for a position behind the last line must be answered \
             with a result, got {answer}"
        );
    }

    lsp.assert_still_serving(uri, "behind_last_line");
    assert_eq!(lsp.shut_down(), Some(0));
}

// ── The language server and a handler that fails ─────────────────────

/// A `codeAction` request that carries a diagnostic whose range no
/// longer fits the text: what a client sends when the document changed
/// after the diagnostic was published. Here the range starts one
/// character in front of a two-byte character, and the quick fix for
/// `(a -> b)` looks two bytes ahead.
///
/// Whether the handler copes with that range or fails on it, the server
/// must answer the request, a failure with the code for an internal
/// error (-32603), and must go on serving.
#[test]
fn lsp_survives_a_request_whose_handler_fails() {
    let mut lsp = Lsp::start("stale_diagnostic");
    let uri = "file:///wave1_tooling/stale_diagnostic.silt";
    lsp.open(uri, "fn main() {\n  (xé)\n}\n");

    let range = json!({
        "start": { "line": 1, "character": 3 },
        "end": { "line": 1, "character": 4 }
    });
    let answer = lsp.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri },
            "range": range,
            "context": {
                "diagnostics": [{
                    "range": range,
                    "severity": 1,
                    "message": "expected identifier, found ->"
                }]
            }
        }),
    );
    if let Some(error) = answer.get("error") {
        assert_eq!(
            error["code"],
            json!(-32603),
            "a handler that fails is an internal error: {answer}"
        );
        let message = error["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("textDocument/codeAction"),
            "the error must name the request: {answer}"
        );
        let log = lsp.stderr();
        assert!(
            log.contains("silt-lsp: internal error while handling request")
                && log.contains("textDocument/codeAction"),
            "the failure must be logged to stderr, got:\n{log}"
        );
    } else {
        assert!(
            answer.get("result").is_some(),
            "a response carries a result or an error: {answer}"
        );
    }

    lsp.assert_still_serving(uri, "stale_diagnostic");
    assert_eq!(lsp.shut_down(), Some(0));
}

// ── `silt test --filter` ─────────────────────────────────────────────

/// The same when the files are discovered in a directory. A file that
/// has no test for the filter is left alone; one that does not parse
/// cannot be asked for its tests, and is reported.
#[test]
fn filter_selects_files_by_their_parsed_tests() {
    let mut project = Project::new(
        "filter_directory",
        &[
            (
                "a_test.silt",
                "import test\nfn  test_spaced() {\n  test.assert_eq(1, 1)\n}\n",
            ),
            (
                "b_test.silt",
                "import test\nfn test_other() {\n  test.assert_eq(1, 1)\n}\n",
            ),
            ("c_test.silt", "fn test_broken( {\n"),
        ],
    );
    let out = project.silt(&["test", "--filter", "spaced"]);
    assert_eq!(out.code, Some(1), "{out:#?}");
    assert!(out.stderr.contains("a_test.silt::test_spaced"), "{out:#?}");
    assert!(!out.stderr.contains("test_other"), "{out:#?}");
    assert!(
        out.stderr.contains("error[parse]") && out.stderr.contains("c_test.silt:1:"),
        "{out:#?}"
    );
    assert!(
        out.stderr
            .contains("1 test: 1 passed, 0 failed, 0 skipped (1 file failed to compile)"),
        "{out:#?}"
    );
}

// ── `silt test` and `silt check` report the same ─────────────────────

/// `silt test` and `silt check` print the same diagnostics for files
/// with errors of every phase, and both fail.
#[test]
fn test_and_check_report_the_same_diagnostics() {
    let files: &[(&str, &str)] = &[
        (
            "type_error_test.silt",
            "import test\nfn test_bad() {\n  let n: Int = \"s\"\n  test.assert_eq(n, 1)\n}\n",
        ),
        (
            "not_imported_test.silt",
            "fn test_bad() {\n  test.assert_eq(1, 1)\n}\n",
        ),
        (
            "missing_module_test.silt",
            "import test\nimport no_such_module\nfn test_bad() {\n  test.assert_eq(no_such_module.f(1), 1)\n}\n",
        ),
        (
            "parse_error_test.silt",
            "import test\nfn test_bad() {\n  let = 1\n}\n",
        ),
        (
            "lex_error_test.silt",
            "import test\nfn test_bad() {\n  println(“hello”)\n}\n",
        ),
        (
            "type_error_and_warning_test.silt",
            "import test\nfn test_bad() {\n  let list = [1]\n  let n: Int = \"s\"\n  test.assert_eq(n, 1)\n}\n",
        ),
    ];
    let mut project = Project::new("parity", files);
    for &(file, _) in files {
        let check = project.silt(&["check", file]);
        assert_eq!(check.code, Some(1), "{file}\n{check:#?}");
        assert!(!check.diagnostics().is_empty(), "{file}\n{check:#?}");

        let test = project.silt(&["test", file]);
        assert_eq!(
            test.code,
            Some(1),
            "{file}: a file with errors must fail under `silt test`\n{test:#?}"
        );
        assert_eq!(
            test.diagnostics(),
            check.diagnostics(),
            "{file}: `silt test` must report what `silt check` reports\n{test:#?}"
        );
        assert!(
            !test.stderr.contains("PASS") && !test.stderr.contains("FAIL"),
            "{file}: no test of a file with errors may run\n{test:#?}"
        );
        assert!(
            test.stderr.contains("1 file failed to compile"),
            "{file}\n{test:#?}"
        );
    }
}
