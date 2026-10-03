//! Stage 2, wave 2, sweep lane: behavioural locks.
//!
//! Parts 1, 2 and 4 are golden cases (`tests/golden/lang/*/wave2_sweep__*`);
//! what stays here drives the language server over stdio (3) and runs a
//! file of the repository (5).
//!
//!   1. A type name, and an enum variant name, must start with an
//!      upper-case letter. A lower-case type name is a type variable
//!      wherever a type is written, and a lower-case name in a pattern
//!      binds a variable, so such declarations used to pass `silt check`
//!      and misbehave at run time.
//!   2. The expression-depth limit counts what the user wrote: 2048
//!      operators are accepted and 2049 refused; a method call counts as
//!      two. The error points at the start of the expression.
//!   3. `textDocument/codeAction` with a stale diagnostic next to a
//!      multi-byte character answers normally instead of failing.
//!   4. No builtin produces a negative-zero `Float`.
//!   5. `examples/budget.silt` prints the savings rate as a percentage.
//!
//! Every test runs the built `silt` binary on files in a fresh temporary
//! directory, with a timeout, and asserts on exit status and output.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Upper bound for one run of the binary. A run that exceeds it is
/// killed and reported as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

// The fields are shown in failure messages through `Debug`.
#[allow(dead_code)]
#[derive(Debug)]
struct Outcome {
    /// Exit status; `None` if the process was ended by a signal.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// True if the run exceeded `RUN_TIMEOUT` and was killed.
    timed_out: bool,
}

fn fresh_dir(label: &str) -> PathBuf {
    static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let unique = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("silt_wave2_sweep_{pid}_{unique}_{label}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .replace("\r\n", "\n")
}

/// Run `silt <subcommand> <target>` with the working directory `cwd`.
/// Output goes to files, so a child killed on timeout cannot leave the
/// test blocked on a pipe.
fn run_in(cwd: &Path, out_dir: &Path, subcommand: &str, target: &Path) -> Outcome {
    let out_path = out_dir.join("stdout.txt");
    let err_path = out_dir.join("stderr.txt");
    let out_file = std::fs::File::create(&out_path).expect("create stdout file");
    let err_file = std::fs::File::create(&err_path).expect("create stderr file");
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(subcommand)
        .arg(target)
        .current_dir(cwd)
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
            None if started.elapsed() >= RUN_TIMEOUT => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().expect("wait after kill");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    Outcome {
        code: status.code(),
        stdout: read_text(&out_path),
        stderr: read_text(&err_path),
        timed_out,
    }
}

// ════════════════════════════════════════════════════════════════════
// 3. Code action on a stale diagnostic
// ════════════════════════════════════════════════════════════════════

struct Lsp {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: u64,
}

impl Lsp {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn silt lsp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = channel::<Value>();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut length: Option<usize> = None;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                    if let Some(rest) = line.trim_end().strip_prefix("Content-Length:") {
                        length = rest.trim().parse().ok();
                    }
                }
                let Some(length) = length else { return };
                let mut body = vec![0u8; length];
                if reader.read_exact(&mut body).is_err() {
                    return;
                }
                let Ok(value) = serde_json::from_slice::<Value>(&body) else {
                    return;
                };
                if tx.send(value).is_err() {
                    return;
                }
            }
        });
        let mut lsp = Lsp {
            child,
            stdin,
            rx,
            next_id: 1,
        };
        lsp.request("initialize", json!({ "capabilities": {} }));
        lsp.notify("initialized", json!({}));
        lsp
    }

    fn send(&mut self, message: &Value) {
        let body = serde_json::to_string(message).expect("serialise");
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).expect("write");
        self.stdin.flush().expect("flush");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Send a request and wait (bounded) for its response.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + RUN_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self
                .rx
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("no response to {method} (id {id})"));
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return message;
            }
        }
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": { "uri": uri, "languageId": "silt", "version": 1, "text": text }
            }),
        );
    }

    /// Ask for code actions for one diagnostic carrying the arrow-type
    /// message, placed at `(line, character)`.
    fn arrow_code_action(&mut self, uri: &str, line: u32, character: u32) -> Value {
        let range = json!({
            "start": { "line": line, "character": character },
            "end": { "line": line, "character": character + 2 }
        });
        let diagnostic = json!({
            "range": range,
            "severity": 1,
            "message": "expected identifier, found ->"
        });
        self.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": range,
                "context": { "diagnostics": [diagnostic] }
            }),
        )
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A diagnostic computed for an earlier version of the text can point
/// anywhere, including into a multi-byte character or at the last byte
/// of the file. The server must answer with no actions, not fail.
#[test]
fn a_stale_arrow_diagnostic_next_to_a_multibyte_character_gets_no_action() {
    let mut lsp = Lsp::spawn();
    let uri = "file:///wave2_sweep_stale.silt";
    // Line 1 is `  println("€")`: the euro sign (three bytes in UTF-8)
    // is at UTF-16 column 11, right after `("`.
    lsp.open(uri, "fn main() {\n  println(\"\u{20ac}\")\n}\n");
    for (line, character) in [(1, 11), (1, 12), (2, 0), (2, 1)] {
        let response = lsp.arrow_code_action(uri, line, character);
        assert!(
            response.get("error").is_none(),
            "codeAction at {line}:{character} must not fail\n{response}"
        );
        let actions = response
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            actions.is_empty(),
            "a diagnostic that does not point at `->` gets no action at \
             {line}:{character}\n{response}"
        );
    }
}

// ════════════════════════════════════════════════════════════════════
// 5. examples/budget.silt
// ════════════════════════════════════════════════════════════════════

/// Income is 5150.00 and the net balance 4242.47, so the savings rate
/// is 82.4%. The example used to print 0.8%: `a / b else 0.0 * 100.0`
/// multiplies the fallback, not the ratio.
#[test]
fn the_budget_example_prints_the_savings_rate_as_a_percentage() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = fresh_dir("budget");
    let out = run_in(root, &dir, "run", Path::new("examples/budget.silt"));
    assert!(!out.timed_out, "budget example hung\n{out:?}");
    assert_eq!(out.code, Some(0), "budget example must run\n{out:?}");
    let line = out
        .stdout
        .lines()
        .find(|line| line.contains("Savings Rate:"))
        .unwrap_or_else(|| panic!("no savings rate line\n{out:?}"));
    assert!(
        line.trim_end().ends_with("82.4%"),
        "savings rate must be 82.4%, got {line:?}"
    );
}
