//! Behaviour locks for the compiler lane of fix wave 2.
//!
//! W1  A record pattern `P { x, .. }` in `match` did not match an
//!     anonymous record literal passed where a `P` is expected, and the
//!     builtins that take a `Date` (or another builtin record) refused an
//!     anonymous record of that shape.
//! W2  A decoder given a type it does not decode (`json.parse_list(t, Int)`)
//!     failed only at run time; the direct call is now a compile error.
//! W3  A decoder imported by name (`import json.{ parse }`) got no
//!     compile-time check of its type argument.
//! W4  The run-time decode error for a field without a decoder did not
//!     name the field or the record.
//! W5  The frame-limit compile error only suggested splitting "the
//!     expression", also when many statements cause it.
//! W6  `double.baz()` on a named function (with a trait impl for `Fun`)
//!     failed at run time with "undefined global: double.baz".
//!
//! Every test runs the built `silt` binary on files in a fresh temporary
//! directory and asserts on its exit status and output. Each run has a
//! timeout, so a hang fails the test instead of hanging the suite.
//!
//! Tests whose name starts with `guard_` pin behaviour that was correct
//! before the fix and must stay as it is. All other tests fail without
//! the fix.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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
    let name = format!("silt_wave2_compiler_{pid}_{unique}_{label}");
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn read_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.replace("\r\n", "\n")
}

/// Write `files` (name, content) into a fresh directory and run
/// `silt <subcommand> main.silt` there once.
///
/// Output goes to files outside that directory rather than to pipes, so
/// a child that is killed on timeout cannot leave the test blocked on a
/// read.
fn run_silt(label: &str, subcommand: &str, files: &[(&str, &str)]) -> Outcome {
    let dir = fresh_dir(label);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("create work dir");
    for (name, content) in files {
        std::fs::write(work.join(name), content).expect("write source file");
    }
    let out_path = dir.join("stdout.txt");
    let err_path = dir.join("stderr.txt");
    let out_file = std::fs::File::create(&out_path).expect("create stdout file");
    let err_file = std::fs::File::create(&err_path).expect("create stderr file");

    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(subcommand)
        .arg("main.silt")
        .current_dir(&work)
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
            None if started.elapsed() >= RUN_TIMEOUT => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().expect("wait after kill");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };

    let outcome = Outcome {
        code: status.code(),
        stdout: read_text(&out_path),
        stderr: read_text(&err_path),
        timed_out,
    };
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

/// The program must pass `silt check`, and `silt run` must finish in
/// time, exit with status 0 and print exactly `expected`.
fn assert_prints_files(label: &str, files: &[(&str, &str)], expected: &str) {
    let checked = run_silt(label, "check", files);
    assert!(
        !checked.timed_out,
        "{label}: `silt check` hung and was killed after {RUN_TIMEOUT:?}\n{checked:#?}"
    );
    assert_eq!(
        checked.code,
        Some(0),
        "{label}: the program must pass `silt check`\n{checked:#?}"
    );

    let out = run_silt(label, "run", files);
    assert!(
        !out.timed_out,
        "{label}: the program hung and was killed after {RUN_TIMEOUT:?}\n{out:#?}"
    );
    assert_eq!(
        out.stdout, expected,
        "{label}: the program printed something else than expected\n{out:#?}"
    );
    assert_eq!(
        out.code,
        Some(0),
        "{label}: the program must exit with status 0\n{out:#?}"
    );
}

/// `assert_prints_files` for a program in a single file.
fn assert_prints(label: &str, src: &str, expected: &str) {
    assert_prints_files(label, &[("main.silt", src)], expected);
}

/// `silt <subcommand>` must reject the program with an `error[compile]`
/// diagnostic that contains every one of `needles`, and print nothing on
/// stdout.
fn assert_compile_error_with(label: &str, subcommand: &str, src: &str, needles: &[&str]) {
    let out = run_silt(label, subcommand, &[("main.silt", src)]);
    let ctx = format!("{label}, silt {subcommand}");
    assert!(
        !out.timed_out,
        "{ctx}: hung and was killed after {RUN_TIMEOUT:?}\n{out:#?}"
    );
    assert_ne!(
        out.code,
        Some(0),
        "{ctx}: the program must be rejected\n{out:#?}"
    );
    assert!(
        out.stderr.contains("error[compile]"),
        "{ctx}: expected a compile error on stderr\n{out:#?}"
    );
    for needle in needles {
        assert!(
            out.stderr.contains(needle),
            "{ctx}: the compile error must contain {needle:?}\n{out:#?}"
        );
    }
    assert_eq!(
        out.stdout, "",
        "{ctx}: a rejected program must not print anything\n{out:#?}"
    );
}

/// `silt check` and `silt run` must both reject the program with an
/// `error[compile]` diagnostic that contains every one of `needles`.
fn assert_compile_error(label: &str, src: &str, needles: &[&str]) {
    for subcommand in ["check", "run"] {
        assert_compile_error_with(label, subcommand, src, needles);
    }
}

// ── W1: record patterns and builtins accept an anonymous record ──────

#[test]
fn w1_record_pattern_matches_an_anonymous_record() {
    let src = r#"
type P { x: Int, y: Int }

fn f(p: P) -> Int {
  match p {
    P { x, .. } -> x
  }
}

fn g(p: P) -> Int {
  match p {
    P { x: 0, .. } -> 0
    P { x, y } -> x + y
  }
}

fn k(ps: List(P)) -> Int {
  match ps {
    [P { x, y: 4 }, ..rest] -> x
    _ -> 0 - 1
  }
}

fn main() {
  println(f({x: 3, y: 4}))
  println(g({x: 3, y: 4}))
  println(g({x: 0, y: 4}))
  println(k([{x: 8, y: 4}]))
  println(k([{x: 8, y: 5}]))
}
"#;
    assert_prints("w1_anon_match", src, "3\n7\n0\n8\n-1\n");
}

#[test]
fn w1_date_builtin_accepts_an_anonymous_record() {
    let src = r#"
import time

fn main() {
  println(time.add_days({year: 2024, month: 1, day: 31}, 1))
}
"#;
    assert_prints("w1_anon_date", src, "2024-02-01\n");
}

/// Nominal records keep matching as before, and equality between an
/// anonymous and a nominal record of the same shape is unchanged.
#[test]
fn guard_w1_nominal_record_patterns_and_equality() {
    let src = r#"
type P { x: Int, y: Int }

fn f(p: P) -> Int {
  match p {
    P { x: 1, .. } -> 100
    P { x, .. } -> x
  }
}

fn main() {
  println(f(P { x: 5, y: 6 }))
  println(f(P { x: 1, y: 6 }))
  println({x: 1, y: 2} == P { x: 1, y: 2 })
  println({x: 1, y: 3} == P { x: 1, y: 2 })
}
"#;
    assert_prints("guard_w1_nominal", src, "5\n100\ntrue\nfalse\n");
}

// ── W2: a type the decoder does not take is a compile error ──────────

#[test]
fn w2_json_parse_list_of_a_primitive_is_a_compile_error() {
    let src = r#"
import json

fn main() {
  println(json.parse_list("[1, 2]", Int))
}
"#;
    assert_compile_error(
        "w2_json_parse_list_int",
        src,
        &["json.parse_list", "`Int`", "record type"],
    );
}

#[test]
fn w2_toml_parse_of_a_primitive_is_a_compile_error() {
    let src = r#"
import toml

fn main() {
  println(toml.parse("a = 1", String))
}
"#;
    assert_compile_error(
        "w2_toml_parse_string",
        src,
        &["toml.parse", "`String`", "record type"],
    );
}

#[test]
fn w2_toml_parse_list_of_a_primitive_is_a_compile_error() {
    let src = r#"
import toml

fn main() {
  println(toml.parse_list("a = 1", Bool))
}
"#;
    assert_compile_error(
        "w2_toml_parse_list_bool",
        src,
        &["toml.parse_list", "`Bool`", "record type"],
    );
}

#[test]
fn w2_decoding_into_a_container_type_is_a_compile_error() {
    let src = r#"
import json

fn main() {
  println(json.parse("[1]", List))
}
"#;
    assert_compile_error("w2_json_parse_list_type", src, &["json.parse", "`List`"]);
}

/// The decoders that do take primitive types keep taking them.
#[test]
fn guard_w2_primitive_type_arguments_that_are_supported() {
    let src = r#"
import json
import toml

fn main() {
  println(json.parse("1", Int))
  println(json.parse("2.5", ExtFloat))
  println(json.parse_map("""{"a": 1}""", Int))
  println(toml.parse_map("a = 1", Int))
}
"#;
    assert_prints(
        "guard_w2_primitives",
        src,
        "Ok(1)\nOk(2.5)\nOk(#{\"a\": 1})\nOk(#{\"a\": 1})\n",
    );
}

/// Through a `type a` parameter the type is only known at run time; the
/// decoder still reports it there.
#[test]
fn guard_w2_type_parameter_path_stays_a_run_time_error() {
    let src = r#"
import json

fn decode_list(text: String, type a) -> Result(List(a), JsonError) {
  json.parse_list(text, a)
}

fn main() {
  println(decode_list("[1]", Int))
}
"#;
    let checked = run_silt("guard_w2_type_param", "check", &[("main.silt", src)]);
    assert!(!checked.timed_out, "silt check hung\n{checked:#?}");
    assert_eq!(
        checked.code,
        Some(0),
        "the program must pass `silt check`\n{checked:#?}"
    );
    let out = run_silt("guard_w2_type_param", "run", &[("main.silt", src)]);
    assert!(!out.timed_out, "silt run hung\n{out:#?}");
    assert_ne!(out.code, Some(0), "the run must fail\n{out:#?}");
    assert!(
        out.stderr.contains("error[runtime]")
            && out
                .stderr
                .contains("json.parse_list: type argument must be a record type"),
        "expected the run-time decoder error\n{out:#?}"
    );
}

// ── W3: decoders imported by name are checked ─────────────────────────

#[test]
fn w3_selectively_imported_parse_checks_the_record() {
    let src = r#"
import json.{ parse }

type Bag { name: String, items: Set(Int) }

fn main() {
  match parse("""{"name": "b", "items": [1]}""", Bag) {
    Ok(b) -> println(b.name)
    Err(e) -> println(e.message())
  }
}
"#;
    assert_compile_error(
        "w3_selective_parse",
        src,
        &["json.parse", "Bag", "items", "Set(Int)"],
    );
}

#[test]
fn w3_selectively_imported_parse_is_checked_when_piped() {
    let src = r#"
import json.{ parse }

type Bag { name: String, items: Set(Int) }

fn main() {
  let r = """{"name": "b", "items": [1]}""" |> parse(Bag)
  match r {
    Ok(b) -> println(b.name)
    Err(e) -> println(e.message())
  }
}
"#;
    assert_compile_error(
        "w3_selective_parse_piped",
        src,
        &["json.parse", "Bag", "items", "Set(Int)"],
    );
}

#[test]
fn w3_selectively_imported_parse_list_checks_a_primitive() {
    let src = r#"
import json.{ parse_list }

fn main() {
  println(parse_list("[1]", Int))
}
"#;
    assert_compile_error(
        "w3_selective_parse_list",
        src,
        &["json.parse_list", "`Int`", "record type"],
    );
}

#[test]
fn w3_decoder_called_through_a_module_alias_is_checked() {
    let src = r#"
import json as j

fn main() {
  println(j.parse_list("[1]", Int))
}
"#;
    assert_compile_error(
        "w3_alias_parse_list",
        src,
        &["json.parse_list", "`Int`", "record type"],
    );
}

#[test]
fn guard_w3_selectively_imported_decoders_still_decode() {
    let src = r#"
import json.{ parse, parse_list }

type Point { x: Int, y: Int }

fn main() {
  match parse("""{"x": 1, "y": 2}""", Point) {
    Ok(p) -> println(p.x + p.y)
    Err(e) -> println(e.message())
  }
  match parse_list("""[{"x": 3, "y": 4}]""", Point) {
    Ok(ps) -> println(ps)
    Err(e) -> println(e.message())
  }
}
"#;
    assert_prints("guard_w3_selective_ok", src, "3\n[Point {x: 3, y: 4}]\n");
}

// ── W4: the run-time decode error names the field ─────────────────────

#[test]
fn w4_run_time_decode_error_names_the_field_and_record() {
    let src = r#"
import json
import toml

type Inner { tags: List(Set(Int)) }
type Outer { name: String, inner: Inner }

fn from_json(text: String, type a) -> Result(a, JsonError) {
  json.parse(text, a)
}

fn from_toml(text: String, type a) -> Result(a, TomlError) {
  toml.parse(text, a)
}

fn main() {
  match from_json("""{"name": "o", "inner": {"tags": [[1]]}}""", Outer) {
    Ok(v) -> println(v.name)
    Err(e) -> println(e.message())
  }
  match from_toml("name = \"o\"\n[inner]\ntags = [[1]]\n", Outer) {
    Ok(v) -> println(v.name)
    Err(e) -> println(e.message())
  }
}
"#;
    let line =
        "cannot decode field `tags` of `Inner`: a value of type List(Set(Int)) cannot be decoded\n";
    assert_prints("w4_field_named", src, &format!("{line}{line}"));
}

// ── W5: the frame-limit error covers many statements ──────────────────

/// One function whose statements together keep more values on the
/// stack than a frame can address. No single expression is large.
#[test]
fn w5_frame_limit_error_mentions_statements() {
    const WIDTH: usize = 250;
    const STATEMENTS: usize = 95;
    let mut src = String::from("fn main() {\n");
    let zeros = vec!["0"; WIDTH].join(", ");
    src.push_str(&format!("  let t = ({zeros})\n"));
    for i in 0..STATEMENTS {
        let names: Vec<String> = (0..WIDTH).map(|j| format!("v{i}_{j}")).collect();
        src.push_str(&format!("  let ({}) = t\n", names.join(", ")));
    }
    src.push_str(&format!(
        "  println(v0_0 + v{}_{})\n}}\n",
        STATEMENTS - 1,
        WIDTH - 1
    ));
    assert_compile_error_with(
        "w5_frame_limit",
        "check",
        &src,
        &[
            "values on its stack at once",
            "move some of its statements into separate functions",
        ],
    );
}

// ── W6: a method call on a named function ─────────────────────────────

#[test]
fn w6_method_call_on_a_named_function() {
    let src = r#"
trait Baz { fn baz(self) -> String }
trait Baz for Fun { fn baz(self) -> String = "baz" }

fn double(x: Int) -> Int { x * 2 }

fn main() {
  println(double.baz())
  println(double(21))
}
"#;
    assert_prints("w6_named_fn", src, "baz\n42\n");
}

#[test]
fn w6_method_call_on_an_imported_function() {
    let util = r#"
trait Baz { fn baz(self) -> String }
trait Baz for Fun { fn baz(self) -> String = "baz" }

pub fn triple(x: Int) -> Int { x * 3 }
"#;
    let main = r#"
import util.{ triple }
import util

fn main() {
  println(triple.baz())
  println(util.triple(2))
}
"#;
    assert_prints_files(
        "w6_imported_fn",
        &[("main.silt", main), ("util.silt", util)],
        "baz\n6\n",
    );
}

#[test]
fn w6_method_call_on_a_function_of_the_same_module() {
    let util = r#"
trait Baz { fn baz(self) -> String }
trait Baz for Fun { fn baz(self) -> String = "baz" }

pub fn triple(x: Int) -> Int { x * 3 }

pub fn inner() -> String { triple.baz() }
"#;
    let main = r#"
import util

fn main() {
  println(util.inner())
}
"#;
    assert_prints_files(
        "w6_module_fn",
        &[("main.silt", main), ("util.silt", util)],
        "baz\n",
    );
}

/// Lambdas in lets, and module-qualified calls, work as before.
#[test]
fn guard_w6_lambdas_and_module_calls() {
    let src = r#"
import list

trait Baz { fn baz(self) -> String }
trait Baz for Fun { fn baz(self) -> String = "baz" }

let tl = { x -> x + 1 }

fn main() {
  let f = { x -> x * 3 }
  println(f.baz())
  println(tl.baz())
  println(list.length([1, 2, 3]))
}
"#;
    assert_prints("guard_w6_lambdas", src, "baz\nbaz\n3\n");
}
