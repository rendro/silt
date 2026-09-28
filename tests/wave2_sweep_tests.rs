//! Stage 2, wave 2, sweep lane: behavioural locks.
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

/// Write `src` to `main.silt` in a fresh directory and run
/// `silt <subcommand> main.silt` there.
fn run_program(label: &str, subcommand: &str, src: &str) -> Outcome {
    let dir = fresh_dir(label);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("create work dir");
    std::fs::write(work.join("main.silt"), src).expect("write main.silt");
    let out = run_in(&work, &dir, subcommand, Path::new("main.silt"));
    assert!(!out.timed_out, "{label}: `silt {subcommand}` hung\n{out:?}");
    out
}

/// `silt check` and `silt run` both reject `src` with a parse error
/// whose message contains every one of `needles`.
fn assert_rejected(label: &str, src: &str, needles: &[&str]) {
    for subcommand in ["check", "run"] {
        let out = run_program(label, subcommand, src);
        assert_eq!(
            out.code,
            Some(1),
            "{label}: `silt {subcommand}` must fail\n{out:?}"
        );
        assert!(
            out.stderr.contains("error[parse]"),
            "{label}: `silt {subcommand}` must report a parse error\n{out:?}"
        );
        for needle in needles {
            assert!(
                out.stderr.contains(needle),
                "{label}: `silt {subcommand}` must say {needle:?}\n{out:?}"
            );
        }
        assert!(
            !out.stderr.contains("error[runtime]"),
            "{label}: `silt {subcommand}` must not get as far as running\n{out:?}"
        );
    }
}

/// `silt run` runs `src` and prints exactly `expected`.
fn assert_runs(label: &str, src: &str, expected: &str) {
    let out = run_program(label, "run", src);
    assert_eq!(
        out.code,
        Some(0),
        "{label}: `silt run` must succeed\n{out:?}"
    );
    assert_eq!(out.stdout, expected, "{label}: wrong output\n{out:?}");
}

// ════════════════════════════════════════════════════════════════════
// 1. Type and variant names start with an upper-case letter
// ════════════════════════════════════════════════════════════════════

#[test]
fn a_lowercase_record_type_name_is_rejected() {
    assert_rejected(
        "lc_record",
        "type point { x: Int, y: Int }\n\
         fn main() {\n  let p = point { x: 1, y: 2 }\n  println(p.x)\n}\n",
        &[
            "type name 'point' must start with an uppercase letter",
            "`type Point`",
            "main.silt:1:6",
        ],
    );
}

#[test]
fn a_lowercase_type_alias_name_is_rejected() {
    assert_rejected(
        "lc_alias",
        "type meters = Int\n\
         fn grow(m: meters) -> meters = m + 1\n\
         fn main() {\n  println(grow(1))\n}\n",
        &["type name 'meters' must start with an uppercase letter"],
    );
}

#[test]
fn a_lowercase_enum_type_name_is_rejected() {
    assert_rejected(
        "lc_enum",
        "type color { Red, Green }\n\
         fn main() {\n  println(Green)\n}\n",
        &["type name 'color' must start with an uppercase letter"],
    );
}

/// A type name that starts with `_` resolves as a named type wherever it
/// is written, so it is not refused.
#[test]
fn a_type_name_starting_with_an_underscore_is_accepted() {
    assert_runs(
        "underscore_type",
        "type _Meters = Int\nfn f(m: _Meters) -> Int { m + 1 }\nfn main() { println(f(3)) }\n",
        "4\n",
    );
}

/// A variant that starts with `_` binds a variable in a pattern, so it is
/// refused; the hint suggests the name without the underscore.
#[test]
fn a_variant_starting_with_an_underscore_is_refused_with_a_usable_hint() {
    assert_rejected(
        "underscore_variant",
        "type Color { _Red, Green }\nfn main() { println(1) }\n",
        &[
            "enum variant '_Red' must start with an uppercase letter",
            "e.g. `Red`",
        ],
    );
}

#[test]
fn a_lowercase_enum_variant_is_rejected() {
    assert_rejected(
        "lc_variant",
        "type Color { Red, green }\n\
         fn main() {\n  let c = Red\n  match c {\n    Red -> println(\"red\")\n    green -> println(\"green\")\n  }\n}\n",
        &[
            "enum variant 'green' must start with an uppercase letter",
            "`Green`",
            "main.silt:1:19",
        ],
    );
}

/// A body whose first name is lower case is read as a record, so a
/// lower-case first variant fails as a record field without `:`. The
/// message says what a variant has to look like.
#[test]
fn a_lowercase_first_enum_variant_gets_a_variant_hint() {
    assert_rejected(
        "lc_first_variant",
        "type Color { red, Green }\nfn main() {\n  println(Green)\n}\n",
        &[
            "expected `:` after record field 'red'",
            "variant names start with an uppercase letter",
            "`Red`",
        ],
    );
    assert_rejected(
        "lc_first_variant_fields",
        "type Shape {\n  circle(Float)\n  Square(Float)\n}\nfn main() {\n  println(1)\n}\n",
        &["variant names start with an uppercase letter", "`Circle`"],
    );
}

/// Guard (passes before and after the fix): upper-case types, variants
/// and aliases with lower-case record fields keep working.
#[test]
fn uppercase_type_and_variant_names_keep_working() {
    assert_runs(
        "uc_names",
        "type Point { x: Int, y: Int }\n\
         type Color { Red, Green(Int) }\n\
         type Meters = Int\n\
         fn grow(m: Meters) -> Meters = m + 1\n\
         fn main() {\n  let p = Point { x: 1, y: 2 }\n  let c = Green(3)\n  match c {\n    \
         Red -> println(\"red\")\n    Green(n) -> println(\"green {n} {p.x} {grow(p.y)}\")\n  }\n}\n",
        "green 3 1 3\n",
    );
}

// ════════════════════════════════════════════════════════════════════
// 2. Expression depth
// ════════════════════════════════════════════════════════════════════

/// `let r = x + x + ... + x` with `operators` operators, on line 3; the
/// expression starts in column 11.
fn plus_chain(operators: usize) -> String {
    let tail = " + x".repeat(operators);
    format!("fn main() {{\n  let x = 1\n  let r = x{tail}\n  println(r)\n}}\n")
}

/// `let r = x.same().same()...` with `calls` method calls.
fn method_chain(calls: usize) -> String {
    let tail = ".same()".repeat(calls);
    format!(
        "trait Same {{\n  fn same(self) -> Int\n}}\n\
         trait Same for Int {{\n  fn same(self) -> Int = self\n}}\n\
         fn main() {{\n  let x = 7\n  let r = x{tail}\n  println(r)\n}}\n"
    )
}

#[test]
fn a_chain_of_2048_operators_is_accepted() {
    assert_runs("plus_2048", &plus_chain(2048), "2049\n");
}

#[test]
fn a_chain_of_2049_operators_is_refused_at_the_start_of_the_expression() {
    let out = run_program("plus_2049", "check", &plus_chain(2049));
    assert_eq!(out.code, Some(1), "2049 operators must be refused\n{out:?}");
    assert!(
        out.stderr.contains("expression is too deep")
            && out.stderr.contains("more than 2048 levels")
            && out.stderr.contains("adds two"),
        "the message must state the limit and how it is counted\n{out:?}"
    );
    assert!(
        out.stderr.contains("main.silt:3:11"),
        "the error must point at the start of the expression\n{out:?}"
    );
}

#[test]
fn a_chain_of_1024_method_calls_is_accepted() {
    assert_runs("method_1024", &method_chain(1024), "7\n");
}

#[test]
fn a_chain_of_1025_method_calls_is_refused_and_the_message_says_why() {
    let out = run_program("method_1025", "check", &method_chain(1025));
    assert_eq!(
        out.code,
        Some(1),
        "1025 method calls must be refused\n{out:?}"
    );
    assert!(
        out.stderr.contains("more than 2048 levels")
            && out.stderr.contains("a method call `x.f()` adds two"),
        "the message must explain that a method call counts as two\n{out:?}"
    );
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

/// Guard (passes before and after the fix): a diagnostic that does point
/// at the `->` of `(A -> B)` still gets the rewrite to `Fn(A) -> B`.
#[test]
fn an_arrow_diagnostic_on_the_arrow_still_gets_the_rewrite() {
    let mut lsp = Lsp::spawn();
    let uri = "file:///wave2_sweep_arrow.silt";
    lsp.open(uri, "fn apply(f: (Int -> Int), x: Int) -> Int = f(x)\n");
    // `->` of `(Int -> Int)` is at column 17.
    let response = lsp.arrow_code_action(uri, 0, 17);
    assert!(response.get("error").is_none(), "{response}");
    let text = response.to_string();
    assert!(
        text.contains("Fn(Int) -> Int"),
        "the quick fix must rewrite `(Int -> Int)` to `Fn(Int) -> Int`\n{response}"
    );
}

// ════════════════════════════════════════════════════════════════════
// 4. No negative-zero Float
// ════════════════════════════════════════════════════════════════════

#[test]
fn builtins_never_produce_a_negative_zero_float() {
    assert_runs(
        "negative_zero",
        "import float\nimport math\n\
         fn num(s: String) -> Float {\n  match float.parse(s) {\n    Ok(f) -> f\n    Err(_) -> 1.0\n  }\n}\n\
         fn main() {\n  \
         let a = num(\"-0.0\")\n  println(\"parse {a} {float.to_string(a)}\")\n  \
         let b = num(\"-1e-400\")\n  println(\"underflow {b} {float.to_string(b)}\")\n  \
         let c = math.atan2(num(\"-1e-300\"), num(\"1e300\"))\n  println(\"atan2 {c} {float.to_string(c)}\")\n\
         }\n",
        "parse 0 0.0\nunderflow 0 0.0\natan2 0 0.0\n",
    );
}

/// The Float producers outside numeric.rs: `list.product_float`, and the
/// JSON and TOML decoders of a `Float` field. A `Float` is finite, so a
/// TOML `nan` does not decode into one.
#[test]
fn decoders_and_list_product_never_produce_a_negative_zero_float() {
    assert_runs(
        "negative_zero_producers",
        r#"import float
import list
import json
import toml
type R { x: Float }
fn main() {
  let p = list.product_float([-1.0, 0.0])
  println("product {p} {float.to_string(p)}")
  match json.parse("\{\"x\": -0.0}", R) {
    Ok(r) -> println("json {r.x} {float.to_string(r.x)}")
    Err(e) -> println("json err {e}")
  }
  match toml.parse("x = -0.0", R) {
    Ok(r) -> println("toml {r.x} {float.to_string(r.x)}")
    Err(e) -> println("toml err {e}")
  }
  match toml.parse("x = nan", R) {
    Ok(r) -> println("nan accepted {r.x}")
    Err(_) -> println("nan rejected")
  }
}
"#,
        "product 0 0.0\njson 0 0.0\ntoml 0 0.0\nnan rejected\n",
    );
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
