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

/// One program in `main.silt`, and what `silt check` and `silt run` say
/// about it.
fn check_and_run(label: &str, src: &str) -> (Outcome, Outcome) {
    let mut project = Project::new(label, &[("main.silt", src)]);
    let check = project.silt(&["check", "main.silt"]);
    let run = project.silt(&["run", "main.silt"]);
    (check, run)
}

const NO_MAIN: &str = "program has no main() function";

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
/// multilingual plane, an accented identifier, CRLF line ends, and a
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
        "fn main() {\r\n  let café = §\r\n}\r\n",
        '§',
    );
    assert_survives_rejected_character(
        "behind_byte_order_mark",
        "\u{feff}fn main() {\n  §\n}\n",
        '§',
    );
}

/// Guard: a valid document with non-ASCII text in an identifier, in a
/// string and in a comment is analysed as before.
#[test]
fn lsp_guard_valid_non_ascii_text_has_no_diagnostics() {
    let mut lsp = Lsp::start("valid_non_ascii");
    let uri = "file:///wave1_tooling/valid_non_ascii.silt";
    let text = "-- “quoted” 😀\nfn main() {\n  let café = \"“q” 😀\"\n  café\n}\n";
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

// ── `silt test`: a test that returns `Err` ───────────────────────────

/// The reported case: the `?` returns `Err` from the test, and the
/// assertion behind it never runs.
#[test]
fn test_that_returns_err_through_question_mark_fails() {
    let src = r#"import test
import int
fn test_returns_err() {
  let n = int.parse("not a number")?
  test.assert_eq(n, 999)
  Ok(())
}
"#;
    let mut project = Project::new("err_question_mark", &[("err_test.silt", src)]);
    let out = project.silt(&["test", "err_test.silt"]);
    assert_eq!(
        out.code,
        Some(1),
        "a failed test is exit status 1\n{out:#?}"
    );
    assert!(
        out.stderr.contains("FAIL err_test.silt::test_returns_err"),
        "the test must be reported as failed\n{out:#?}"
    );
    assert!(
        !out.stderr.contains("PASS err_test.silt::test_returns_err"),
        "the test must not be reported as passed\n{out:#?}"
    );
    assert!(
        out.stderr.contains("test_returns_err returned Err: "),
        "the report must say that the test returned Err\n{out:#?}"
    );
    assert!(
        out.stderr.contains("1 test: 0 passed, 1 failed, 0 skipped"),
        "the summary must count the failure\n{out:#?}"
    );
}

/// The report shows the payload of the `Err`.
#[test]
fn test_that_returns_err_shows_the_payload() {
    let src = r#"fn test_plain_err() -> Result(Int, String) {
  Err("the disk is full")
}
"#;
    let mut project = Project::new("err_payload", &[("err_test.silt", src)]);
    let out = project.silt(&["test", "err_test.silt"]);
    assert_eq!(
        out.code,
        Some(1),
        "a failed test is exit status 1\n{out:#?}"
    );
    assert!(
        out.stderr.contains("FAIL err_test.silt::test_plain_err"),
        "the test must be reported as failed\n{out:#?}"
    );
    assert!(
        out.stderr
            .contains("error[runtime]: test_plain_err returned Err: the disk is full"),
        "the report must show the payload of the Err\n{out:#?}"
    );
}

/// One test that returns `Err` fails; the tests next to it are run and
/// counted as before.
#[test]
fn test_that_returns_err_does_not_affect_its_neighbours() {
    let src = r#"import test
fn test_first() {
  test.assert_eq(1, 1)
}
fn test_gives_up() -> Result(Int, String) {
  Err("gave up")
}
fn test_last() {
  test.assert_eq(2, 2)
}
"#;
    let mut project = Project::new("err_neighbours", &[("err_test.silt", src)]);
    let out = project.silt(&["test", "err_test.silt"]);
    assert_eq!(
        out.code,
        Some(1),
        "a failed test is exit status 1\n{out:#?}"
    );
    for line in [
        "PASS err_test.silt::test_first",
        "FAIL err_test.silt::test_gives_up",
        "PASS err_test.silt::test_last",
        "3 tests: 2 passed, 1 failed, 0 skipped",
    ] {
        assert!(out.stderr.contains(line), "expected `{line}`\n{out:#?}");
    }
}

/// Guard: a test that returns `Ok(_)`, Unit, or any other value passes.
#[test]
fn guard_test_that_returns_another_value_passes() {
    let src = r#"import test
type Outcome { Fine, Bad(String) }
fn test_ok() -> Result(Int, String) {
  Ok(1)
}
fn test_unit() {
  test.assert_eq(1, 1)
}
fn test_int() {
  42
}
fn test_some() {
  Some(1)
}
fn test_none() {
  None
}
fn test_user_variant() {
  Bad("not a Result")
}
"#;
    let mut project = Project::new("other_values", &[("values_test.silt", src)]);
    let out = project.silt(&["test", "values_test.silt"]);
    assert_eq!(out.code, Some(0), "all of these tests pass\n{out:#?}");
    assert!(
        out.stderr
            .contains("6 tests: 6 passed, 0 failed, 0 skipped"),
        "all of these tests pass\n{out:#?}"
    );
}

// ── `main`, as the parser sees it ────────────────────────────────────

/// The reported case: two spaces between `fn` and `main`.
#[test]
fn check_finds_main_whatever_the_spacing() {
    for (label, src) in [
        ("two_spaces", "fn  main() {\n  println(\"ran\")\n}\n"),
        ("line_breaks", "fn\nmain\n() {\n  println(\"ran\")\n}\n"),
        ("pub_spaced", "pub  fn   main() {\n  println(\"ran\")\n}\n"),
    ] {
        let (check, run) = check_and_run(label, src);
        assert_eq!(run.code, Some(0), "{label}: the program runs\n{run:#?}");
        assert_eq!(run.stdout, "ran\n", "{label}: the program runs\n{run:#?}");
        assert_eq!(
            check.code,
            Some(0),
            "{label}: `silt check` must accept what `silt run` runs\n{check:#?}"
        );
        assert!(
            !check.stderr.contains(NO_MAIN),
            "{label}: the program has a main\n{check:#?}"
        );
    }
}

/// The reported case: the only `fn main() {}` of the file is inside a
/// block comment, or inside a string.
#[test]
fn check_does_not_take_a_comment_or_a_string_for_main() {
    for (label, src) in [
        ("block_comment", "{-\nfn main() {}\n-}\nfn helper() { 1 }\n"),
        (
            "string",
            "let s = \"\"\"\nfn main() {}\n\"\"\"\nfn helper() { s }\n",
        ),
    ] {
        let (check, run) = check_and_run(label, src);
        for (command, out) in [("check", &check), ("run", &run)] {
            assert_eq!(
                out.code,
                Some(1),
                "{label}: `silt {command}` must reject a program without main\n{out:#?}"
            );
            assert!(
                out.stderr.contains(NO_MAIN),
                "{label}: `silt {command}` must say that there is no main\n{out:#?}"
            );
        }
    }
}

/// A `pub fn` or a test function inside a comment makes the file
/// neither a library module nor a test file.
#[test]
fn check_does_not_take_a_comment_for_a_declaration() {
    for (label, src) in [
        (
            "pub_fn",
            "{-\npub fn double(x) { x * 2 }\n-}\nfn helper() { 1 }\n",
        ),
        ("test_fn", "{-\nfn test_x() { 1 }\n-}\nfn helper() { 1 }\n"),
    ] {
        let (check, run) = check_and_run(label, src);
        for (command, out) in [("check", &check), ("run", &run)] {
            assert_eq!(
                out.code,
                Some(1),
                "{label}: `silt {command}` must reject a program without main\n{out:#?}"
            );
            assert!(
                out.stderr.contains(NO_MAIN),
                "{label}: `silt {command}` must say that there is no main\n{out:#?}"
            );
            assert!(
                !out.stderr.contains("silt test"),
                "{label}: this is not a test file\n{out:#?}"
            );
        }
    }
}

/// `main` is looked up by name when the program starts, so a `let` and
/// an import provide it as well as a `fn`. `silt check` must accept
/// what `silt run` runs.
#[test]
fn check_accepts_a_main_bound_by_let_or_import() {
    let mut project = Project::new(
        "main_by_let_or_import",
        &[
            ("by_let.silt", "let main = fn() { println(\"from let\") }\n"),
            ("by_import.silt", "import helper.{ main }\n"),
            (
                "helper.silt",
                "pub fn main() { println(\"from helper\") }\n",
            ),
        ],
    );
    for (file, printed) in [
        ("by_let.silt", "from let\n"),
        ("by_import.silt", "from helper\n"),
    ] {
        let run = project.silt(&["run", file]);
        assert_eq!(run.code, Some(0), "{file}: the program runs\n{run:#?}");
        assert_eq!(run.stdout, printed, "{file}: the program runs\n{run:#?}");
        let check = project.silt(&["check", file]);
        assert_eq!(
            check.code,
            Some(0),
            "{file}: `silt check` must accept what `silt run` runs\n{check:#?}"
        );
    }
}

/// A program without `main` is rejected before any of it runs.
#[test]
fn run_rejects_a_program_without_main_before_running_it() {
    let (check, run) = check_and_run("side_effect", "let x = println(\"side effect\")\n");
    for (command, out) in [("check", &check), ("run", &run)] {
        assert_eq!(out.code, Some(1), "`silt {command}`\n{out:#?}");
        assert!(out.stderr.contains(NO_MAIN), "`silt {command}`\n{out:#?}");
    }
    assert_eq!(
        run.stdout, "",
        "a program that is rejected must not have run\n{run:#?}"
    );
}

/// Guard: a library module and a test file pass `silt check` without a
/// `main`; `silt run` rejects both, and points a test file to
/// `silt test`.
#[test]
fn guard_library_modules_and_test_files_need_no_main() {
    let mut project = Project::new(
        "no_main_needed",
        &[
            ("lib.silt", "pub fn double(x: Int) -> Int { x * 2 }\n"),
            (
                "unit_test.silt",
                "import test\nfn test_a() {\n  test.assert_eq(1, 1)\n}\n",
            ),
            ("plain.silt", "fn helper() { 1 }\n"),
        ],
    );
    for file in ["lib.silt", "unit_test.silt"] {
        let check = project.silt(&["check", file]);
        assert_eq!(check.code, Some(0), "{file}\n{check:#?}");
        let run = project.silt(&["run", file]);
        assert_eq!(run.code, Some(1), "{file}\n{run:#?}");
        assert!(run.stderr.contains(NO_MAIN), "{file}\n{run:#?}");
    }
    let run = project.silt(&["run", "unit_test.silt"]);
    assert!(
        run.stderr
            .contains("This looks like a test file — run it with 'silt test unit_test.silt'"),
        "a test file gets a pointer to `silt test`\n{run:#?}"
    );
    let check = project.silt(&["check", "plain.silt"]);
    assert_eq!(check.code, Some(1), "plain.silt\n{check:#?}");
    assert!(check.stderr.contains(NO_MAIN), "plain.silt\n{check:#?}");
}

// ── `main` with parameters ───────────────────────────────────────────

/// The reported case. `silt check` accepted the program, and `silt run`
/// failed when it called `main` without arguments.
#[test]
fn main_with_parameters_is_a_check_time_error() {
    let (check, run) = check_and_run("main_one_parameter", "fn main(x: Int) { x }\n");
    for (command, out) in [("check", &check), ("run", &run)] {
        assert_eq!(out.code, Some(1), "`silt {command}`\n{out:#?}");
        assert!(
            out.stderr.contains(
                "error[compile]: the entry point 'main' must take no parameters, \
                 but it declares 1 parameter"
            ),
            "`silt {command}` must report the parameter of main\n{out:#?}"
        );
        assert!(
            out.stderr.contains("main.silt:1:9"),
            "`silt {command}` must point at the parameter\n{out:#?}"
        );
        assert!(
            !out.stderr.contains("error[runtime]"),
            "`silt {command}`: the program must not have been started\n{out:#?}"
        );
    }
    assert_eq!(check.diagnostics(), run.diagnostics());
}

/// The same for several parameters, for a `main` bound to a closure,
/// and in the JSON output of `silt check`.
#[test]
fn main_with_parameters_in_other_forms() {
    let (check, run) = check_and_run("main_two_parameters", "fn main(a: Int, b: Int) { a + b }\n");
    for out in [&check, &run] {
        assert_eq!(out.code, Some(1), "{out:#?}");
        assert!(out.stderr.contains("it declares 2 parameters"), "{out:#?}");
    }

    let (check, run) = check_and_run("main_closure", "let main = { a, b -> a + b }\n");
    for out in [&check, &run] {
        assert_eq!(out.code, Some(1), "{out:#?}");
        assert!(out.stderr.contains("it declares 2 parameters"), "{out:#?}");
    }

    let mut project = Project::new("main_json", &[("main.silt", "fn main(x: Int) { x }\n")]);
    let out = project.silt(&["check", "--format", "json", "main.silt"]);
    assert_eq!(out.code, Some(1), "{out:#?}");
    let reported: Value = serde_json::from_str(out.stdout.trim()).expect("JSON on stdout");
    let reported = reported.as_array().expect("a list of diagnostics");
    assert_eq!(reported.len(), 1, "{out:#?}");
    assert_eq!(reported[0]["kind"], json!("compile"), "{out:#?}");
    assert_eq!(reported[0]["severity"], json!("error"), "{out:#?}");
    assert_eq!(reported[0]["line"], json!(1), "{out:#?}");
    assert_eq!(reported[0]["col"], json!(9), "{out:#?}");
    let message = reported[0]["message"].as_str().unwrap_or_default();
    assert!(message.contains("must take no parameters"), "{out:#?}");
}

/// Library modules and test files are not entry points, so a `main` with
/// parameters in them is not an error: `silt check` accepts the library
/// and `silt test` runs the test file's tests.
#[test]
fn main_with_parameters_in_a_library_or_test_file_is_not_an_entry_point_error() {
    let mut project = Project::new(
        "main_param_library",
        &[
            (
                "lib.silt",
                "pub fn greet() -> String { \"hi\" }\npub fn main(x: Int) { x }\n",
            ),
            (
                "param_test.silt",
                "fn main(args: List(String)) { () }\nfn test_a() { 1 }\n",
            ),
        ],
    );
    let check = project.silt(&["check", "lib.silt"]);
    assert_eq!(check.code, Some(0), "{check:#?}");
    assert!(
        !check.stderr.contains("must take no parameters"),
        "{check:#?}"
    );
    let test = project.silt(&["test", "param_test.silt"]);
    assert_eq!(test.code, Some(0), "{test:#?}");
    assert!(test.stderr.contains("PASS"), "{test:#?}");
    assert!(
        !test.stderr.contains("must take no parameters"),
        "{test:#?}"
    );
}

/// Guard: a `main` without parameters and a function with parameters
/// that is not `main` are accepted.
#[test]
fn guard_other_signatures_are_accepted() {
    let src = "fn main_helper(x: Int) { x }\nfn main() {\n  println(main_helper(1))\n}\n";
    let (check, run) = check_and_run("other_signatures", src);
    assert_eq!(check.code, Some(0), "{check:#?}");
    assert_eq!(check.stderr, "", "{check:#?}");
    assert_eq!(run.code, Some(0), "{run:#?}");
    assert_eq!(run.stdout, "1\n", "{run:#?}");
}

// ── `silt test --filter` ─────────────────────────────────────────────

/// The reported case: two spaces between `fn` and the name of the test.
/// Without a filter the test ran, with a filter it was not found.
#[test]
fn filter_finds_a_test_whatever_the_spacing() {
    let src = "import test\nfn  test_spaced() {\n  test.assert_eq(1, 1)\n}\n";
    let mut project = Project::new("filter_spaced", &[("spaced_test.silt", src)]);

    let plain = project.silt(&["test", "spaced_test.silt"]);
    assert_eq!(plain.code, Some(0), "{plain:#?}");
    assert!(
        plain.stderr.contains("PASS spaced_test.silt::test_spaced"),
        "{plain:#?}"
    );

    let filtered = project.silt(&["test", "--filter", "spaced", "spaced_test.silt"]);
    assert_eq!(filtered.code, Some(0), "{filtered:#?}");
    assert!(
        filtered
            .stderr
            .contains("PASS spaced_test.silt::test_spaced"),
        "the filter must select the test that runs without it\n{filtered:#?}"
    );
    assert!(
        filtered
            .stderr
            .contains("1 test: 1 passed, 0 failed, 0 skipped"),
        "{filtered:#?}"
    );
    assert!(
        !filtered.stdout.contains("no matching test files found"),
        "{filtered:#?}"
    );
}

/// The same when the files are discovered in a directory. A file that
/// has no test for the filter is left alone, also when it does not
/// parse.
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
    assert_eq!(out.code, Some(0), "{out:#?}");
    assert!(out.stderr.contains("a_test.silt::test_spaced"), "{out:#?}");
    assert!(!out.stderr.contains("test_other"), "{out:#?}");
    assert!(!out.stderr.contains("c_test.silt"), "{out:#?}");
    assert!(
        out.stderr.contains("1 test: 1 passed, 0 failed, 0 skipped"),
        "{out:#?}"
    );
}

/// A test function inside a comment is not a test: a filter that
/// matches only its name selects nothing.
#[test]
fn filter_does_not_take_a_comment_for_a_test() {
    let src = "import test\n{-\nfn test_commented() { 1 }\n-}\nfn test_real() {\n  test.assert_eq(1, 1)\n}\n";
    let mut project = Project::new("filter_comment", &[("c_test.silt", src)]);
    let out = project.silt(&["test", "--filter", "commented", "c_test.silt"]);
    assert_eq!(out.code, Some(0), "{out:#?}");
    assert!(
        out.stdout.contains("no matching test files found"),
        "{out:#?}"
    );
    assert!(!out.stderr.contains("0 tests"), "{out:#?}");
}

/// Guard: what a filter selects and skips in well-formed files.
#[test]
fn guard_filter_selects_by_name() {
    let src = r#"import test
fn test_add_small() {
  test.assert_eq(1 + 1, 2)
}
fn test_subtract() {
  test.assert_eq(5 - 3, 2)
}
pub fn test_add_big() {
  test.assert_eq(100 + 200, 300)
}
fn skip_test_add_later() {
  test.assert(false)
}
"#;
    let mut project = Project::new("filter_guard", &[("f_test.silt", src)]);

    let out = project.silt(&["test", "--filter", "add", "f_test.silt"]);
    assert_eq!(out.code, Some(0), "{out:#?}");
    for line in [
        "PASS f_test.silt::test_add_small",
        "PASS f_test.silt::test_add_big",
        "SKIP f_test.silt::skip_test_add_later",
        "3 tests: 2 passed, 0 failed, 1 skipped",
    ] {
        assert!(out.stderr.contains(line), "expected `{line}`\n{out:#?}");
    }
    assert!(!out.stderr.contains("test_subtract"), "{out:#?}");

    let out = project.silt(&["test", "--filter", "no_such_test", "f_test.silt"]);
    assert_eq!(out.code, Some(0), "{out:#?}");
    assert!(
        out.stdout.contains("no matching test files found"),
        "{out:#?}"
    );

    // A file that does not parse and has a test for the filter is
    // reported.
    let mut project = Project::new("filter_broken", &[("c_test.silt", "fn test_broken( {\n")]);
    let out = project.silt(&["test", "--filter", "broken", "c_test.silt"]);
    assert_eq!(out.code, Some(1), "{out:#?}");
    assert!(out.stderr.contains("error[parse]"), "{out:#?}");
    assert!(out.stderr.contains("1 file failed to compile"), "{out:#?}");
}

// ── `silt test` and `silt check` report the same ─────────────────────

/// The reported case: the test file imports a module with a type error.
#[test]
fn test_reports_the_type_errors_of_imported_modules() {
    let mut project = Project::new(
        "broken_import",
        &[
            (
                "main_test.silt",
                "import test\nimport helper\nfn test_double() {\n  test.assert_eq(helper.double(2), 4)\n}\n",
            ),
            (
                "helper.silt",
                "pub fn double(x: Int) -> Int { x * 2 }\npub fn broken() -> Int { \"not an int\" }\n",
            ),
        ],
    );
    let check = project.silt(&["check", "main_test.silt"]);
    assert_eq!(check.code, Some(1), "{check:#?}");
    assert_eq!(
        check.diagnostics(),
        ["error[type]: type mismatch: expected Int, got String"],
        "{check:#?}"
    );

    let test = project.silt(&["test", "main_test.silt"]);
    assert_eq!(
        test.code,
        Some(1),
        "a file with errors must fail under `silt test`\n{test:#?}"
    );
    assert_eq!(
        test.diagnostics(),
        check.diagnostics(),
        "`silt test` must report what `silt check` reports\n{test:#?}"
    );
    assert!(
        test.stderr.contains("helper.silt:2:24"),
        "the error must point into the imported module\n{test:#?}"
    );
    assert!(
        !test.stderr.contains("PASS"),
        "no test of a file with errors may be reported as passed\n{test:#?}"
    );
    assert!(
        test.stderr.contains("1 file failed to compile"),
        "{test:#?}"
    );
}

/// The reported case: the compiler's warnings.
#[test]
fn test_reports_the_warnings_of_the_compiler() {
    let src = "import test\nfn test_shadow() {\n  let list = [1, 2, 3]\n  test.assert_eq(list, [1, 2, 3])\n}\n";
    let mut project = Project::new("warning", &[("warn_test.silt", src)]);
    let check = project.silt(&["check", "warn_test.silt"]);
    assert_eq!(check.code, Some(0), "{check:#?}");
    let warnings = check.diagnostics();
    assert_eq!(warnings.len(), 1, "{check:#?}");
    assert!(
        warnings[0].starts_with("warning[compile]: variable 'list' shadows the builtin"),
        "{check:#?}"
    );

    let test = project.silt(&["test", "warn_test.silt"]);
    assert_eq!(test.code, Some(0), "a warning fails nothing\n{test:#?}");
    assert_eq!(
        test.diagnostics(),
        warnings,
        "`silt test` must report what `silt check` reports\n{test:#?}"
    );
    assert!(
        test.stderr.contains("PASS warn_test.silt::test_shadow"),
        "{test:#?}"
    );
}

/// A test file that imports a declared dependency. `silt test` checked
/// no name of such a file; an undefined name passed as long as the code
/// around it did not run.
#[test]
fn test_checks_names_in_a_file_that_imports_a_dependency() {
    let mut project = Project::new(
        "dependency",
        &[
            (
                "app/silt.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmathutil = { path = \"../mathutil\" }\n",
            ),
            (
                "app/src/main_test.silt",
                "import mathutil\nimport test\n\nfn test_double() {\n  test.assert_eq(mathutil.double(2), 4)\n}\n\nfn test_dead_arm() {\n  match 1 {\n    2 -> zzz_totally_undefined(1)\n    _ -> ()\n  }\n}\n",
            ),
            (
                "mathutil/silt.toml",
                "[package]\nname = \"mathutil\"\nversion = \"0.1.0\"\n",
            ),
            (
                "mathutil/src/lib.silt",
                "pub fn double(x: Int) -> Int {\n  x * 2\n}\n",
            ),
        ],
    );
    let check = project.silt_in("app", &["check", "src/main_test.silt"]);
    assert_eq!(check.code, Some(1), "{check:#?}");
    assert_eq!(
        check.diagnostics(),
        ["error[type]: undefined variable 'zzz_totally_undefined'"],
        "{check:#?}"
    );

    let test = project.silt_in("app", &["test", "src/main_test.silt"]);
    assert_eq!(
        test.code,
        Some(1),
        "a file with errors must fail under `silt test`\n{test:#?}"
    );
    assert_eq!(
        test.diagnostics(),
        check.diagnostics(),
        "`silt test` must report what `silt check` reports\n{test:#?}"
    );
    assert!(!test.stderr.contains("PASS"), "{test:#?}");
}

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

/// Guard: a test file without errors and warnings prints no diagnostic,
/// and its tests run as before, also the ones that fail.
#[test]
fn guard_clean_test_file_runs_as_before() {
    let src = r#"import test
import helper
fn test_passes() {
  test.assert_eq(helper.double(2), 4)
}
fn test_fails() {
  test.assert_eq(helper.double(2), 5)
}
fn skip_test_later() {
  test.assert(false)
}
"#;
    let mut project = Project::new(
        "clean",
        &[
            ("clean_test.silt", src),
            ("helper.silt", "pub fn double(x: Int) -> Int { x * 2 }\n"),
        ],
    );
    let check = project.silt(&["check", "clean_test.silt"]);
    assert_eq!(check.code, Some(0), "{check:#?}");
    assert_eq!(check.stderr, "", "{check:#?}");

    let test = project.silt(&["test", "clean_test.silt"]);
    assert_eq!(test.code, Some(1), "one test fails\n{test:#?}");
    assert!(
        test.diagnostics()
            .iter()
            .all(|d| d.starts_with("error[runtime]"))
    );
    for line in [
        "PASS clean_test.silt::test_passes",
        "FAIL clean_test.silt::test_fails",
        "SKIP clean_test.silt::skip_test_later",
        "3 tests: 1 passed, 1 failed, 1 skipped",
    ] {
        assert!(test.stderr.contains(line), "expected `{line}`\n{test:#?}");
    }
}
