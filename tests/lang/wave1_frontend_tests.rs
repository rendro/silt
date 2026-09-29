//! Behaviour locks for the front end: formatter self-check, expression
//! height limit, and "expression followed by a block" headers.
//!
//! 1. `silt fmt` checks its own result before it writes. If the result
//!    would not parse, would be a different program, or would not carry
//!    the same comments, the file stays byte-for-byte as it was, the
//!    refusal is printed with the file name, and the exit status is 1.
//!    `silt fmt --check` reports the same refusal with status 2, apart
//!    from "would reformat" (status 1). Programs that the formatter
//!    handles correctly format as before, including every spelling that
//!    the formatter changes on purpose.
//!
//! 2. An expression tree has a maximum height. A chain of operators,
//!    pipes, calls or field accesses that exceeds it is a parse error
//!    with a position, not a stack overflow in a later pass.
//!
//! 3. In a `loop` header a binding initialiser may end in a constructor
//!    (`loop i = n, acc = Nil { ... }`). In a `match` scrutinee a
//!    trailing closure may be used inside parentheses or call arguments,
//!    and the match body may follow the right operand of `|>` directly
//!    (`match xs |> list.head { ... }`).
//!
//! The run-only tests of part 3 are golden cases
//! (`tests/golden/lang/control/wave1_frontend__*`); what stays here runs
//! `silt fmt` (which rewrites the file), or generates chains of 10,000
//! links and runs `check`, `run` and `fmt` on each.
//!
//! Every test runs the built `silt` binary on files in a fresh temporary
//! directory and asserts on its exit status, its output and the files.
//! Each run has a timeout, so a hang fails the test instead of hanging
//! the suite.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Upper bound for one run of the binary. A run that exceeds it is
/// killed and reported as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

/// The name under which every test writes its program.
const MAIN: &str = "main.silt";

#[derive(Debug)]
struct Outcome {
    /// Exit status; `None` if the process was ended by a signal.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// True if the run exceeded `RUN_TIMEOUT` and was killed.
    timed_out: bool,
}

/// A fresh temporary directory that is removed when the value is dropped.
/// Programs live in `work/`; the output of each run goes to files next
/// to it rather than to pipes, so a child that is killed on timeout
/// cannot leave the test blocked on a read.
struct Workspace {
    dir: PathBuf,
    work: PathBuf,
}

impl Workspace {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("silt_wave1_frontend_{pid}_{unique}_{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("create work dir");
        Workspace { dir, work }
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.work.join(name), content).expect("write source file");
    }

    fn read_bytes(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.work.join(name)).expect("read source file")
    }

    fn read(&self, name: &str) -> String {
        String::from_utf8(self.read_bytes(name)).expect("source file is UTF-8")
    }

    /// Run `silt <args>` with `work/` as the current directory.
    fn silt(&self, args: &[&str]) -> Outcome {
        let out_path = self.dir.join("stdout.txt");
        let err_path = self.dir.join("stderr.txt");
        let out_file = std::fs::File::create(&out_path).expect("create stdout file");
        let err_file = std::fs::File::create(&err_path).expect("create stderr file");

        let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .args(args)
            .current_dir(&self.work)
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
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };

        let read = |path: &PathBuf| {
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .replace("\r\n", "\n")
        };
        Outcome {
            code: status.code(),
            stdout: read(&out_path),
            stderr: read(&err_path),
            timed_out,
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A run that ended by itself: no timeout, no signal, no Rust panic and
/// no stack overflow.
fn assert_ended_cleanly(label: &str, what: &str, out: &Outcome) {
    assert!(
        !out.timed_out,
        "{label}: `{what}` did not finish within {RUN_TIMEOUT:?}\n{out:?}"
    );
    assert!(
        out.code.is_some(),
        "{label}: `{what}` was ended by a signal\n{out:?}"
    );
    for marker in ["overflowed its stack", "stack overflow", "panicked at"] {
        assert!(
            !out.stderr.contains(marker),
            "{label}: `{what}` must not report `{marker}`\n{out:?}"
        );
    }
}

// ════════════════════════════════════════════════════════════════════
// 1. The formatter refuses a result that fails its self-check
// ════════════════════════════════════════════════════════════════════

/// `silt fmt` on `src` must refuse: status 1, the file byte-for-byte as
/// it was, and on stderr the refusal with the file name and `reason`.
/// `silt fmt --check` must report the same refusal with status 2 and
/// must not call the file "not formatted".
fn assert_fmt_refuses(label: &str, src: &str, reason: &str) {
    let ws = Workspace::new(label);
    ws.write(MAIN, src);

    let fmt = ws.silt(&["fmt", MAIN]);
    assert_ended_cleanly(label, "silt fmt", &fmt);
    assert_eq!(
        ws.read_bytes(MAIN),
        src.as_bytes(),
        "{label}: `silt fmt` must leave the file untouched\n{fmt:?}"
    );
    assert_eq!(fmt.code, Some(1), "{label}: `silt fmt` must fail\n{fmt:?}");
    assert!(
        fmt.stderr.contains("formatting refused"),
        "{label}: `silt fmt` must say that it refused\n{fmt:?}"
    );
    assert!(
        fmt.stderr.contains(MAIN),
        "{label}: the refusal must name the file\n{fmt:?}"
    );
    assert!(
        fmt.stderr.contains(reason),
        "{label}: the refusal must say `{reason}`\n{fmt:?}"
    );
    assert!(
        fmt.stderr.contains("left unchanged"),
        "{label}: the refusal must say that the file was left unchanged\n{fmt:?}"
    );

    let check = ws.silt(&["fmt", "--check", MAIN]);
    assert_ended_cleanly(label, "silt fmt --check", &check);
    assert_eq!(
        check.code,
        Some(2),
        "{label}: `silt fmt --check` must report a failure (2), not drift (1)\n{check:?}"
    );
    assert!(
        check.stderr.contains("formatting refused") && check.stderr.contains(reason),
        "{label}: `silt fmt --check` must report the refusal\n{check:?}"
    );
    assert!(
        !check.stderr.contains("not formatted"),
        "{label}: a refusal is not \"would reformat\"\n{check:?}"
    );
    assert_eq!(
        ws.read_bytes(MAIN),
        src.as_bytes(),
        "{label}: `silt fmt --check` must leave the file untouched"
    );
}

/// A trailing comment on a line that ends in `} }`: the printer puts the
/// comment between the two braces, which comments out the second one.
#[test]
fn fmt_refuses_when_a_comment_would_swallow_a_closing_brace() {
    assert_fmt_refuses(
        "swallow_brace",
        r#"import list
fn main() {
  let found = [1, 2] |> list.find { w -> match w {
    1 -> true
    _ -> false
  } } -- note
  println("{found}")
}
"#,
        "the result would not parse",
    );
}

/// A trailing comment that the printer puts in front of the comma that
/// separates two call arguments.
#[test]
fn fmt_refuses_when_a_comment_would_swallow_a_comma() {
    assert_fmt_refuses(
        "swallow_comma",
        r#"import option
import map
fn main() {
  let m = #{"a": 1}
  let v = option.unwrap_or(map.get(m, "a") -- look up
    |> option.map { x -> x + 1 }, 0) -- default
  println("{v}")
}
"#,
        "the result would not parse",
    );
}

/// A lambda with a typed parameter as the last argument is printed as a
/// trailing closure, and a closure cannot have a typed parameter.
#[test]
fn fmt_refuses_a_typed_lambda_in_last_argument_position() {
    assert_fmt_refuses(
        "typed_lambda",
        r#"import list
fn main() {
  let ys = list.map([1, 2], fn(x: Int) { x + 1 })
  println("{ys}")
}
"#,
        "the result would not parse",
    );
}

/// A lambda argument in a match scrutinee is printed as a trailing
/// closure, which a scrutinee cannot hold.
#[test]
fn fmt_refuses_a_lambda_argument_in_a_match_scrutinee() {
    assert_fmt_refuses(
        "lambda_in_scrutinee",
        r#"fn run(f) = f()
fn main() {
  let r = match run(fn() { 1 }) {
    1 -> "one"
    _ -> "other"
  }
  println(r)
}
"#,
        "the result would not parse",
    );
}

/// `(a == b) |> show` is printed without the parentheses, which is
/// `a == (b |> show)`.
#[test]
fn fmt_refuses_when_dropped_parentheses_would_regroup_a_pipe() {
    assert_fmt_refuses(
        "pipe_regroup",
        r#"fn show(x) = "{x}"
fn main() {
  let a = 1
  let b = 2
  let s = (a == b) |> show
  println(s)
}
"#,
        "the result would change the program: function `main`",
    );
}

/// The float range pattern `1.0..10.0` is printed as the integer range
/// `1..10`.
#[test]
fn fmt_refuses_when_a_float_range_pattern_would_become_an_int_range() {
    assert_fmt_refuses(
        "float_range",
        r#"fn main() {
  let z = match 1.5 {
    1.0..10.0 -> "in"
    _ -> "out"
  }
  println(z)
}
"#,
        "the result would change the program: function `main`",
    );
}

/// Line comments in places where the printer does not look for one.
#[test]
fn fmt_refuses_when_a_line_comment_would_be_lost() {
    assert_fmt_refuses(
        "after_eq",
        "fn main() {\n  let x = -- why one\n    1\n  println(\"{x}\")\n}\n",
        "the result would lose the comment `-- why one`",
    );
    assert_fmt_refuses(
        "after_arrow",
        r#"fn main() {
  let s = match 1 {
    1 -> -- the one case
      "one"
    _ -> "other"
  }
  println(s)
}
"#,
        "the result would lose the comment `-- the one case`",
    );
    assert_fmt_refuses(
        "fn_header",
        "fn main() -- entry point\n{\n  println(\"hi\")\n}\n",
        "the result would lose the comment `-- entry point`",
    );
    assert_fmt_refuses(
        "type_open",
        r#"type P { -- a point
  x: Int,
  y: Int,
}
fn main() {
  let p = P { x: 1, y: 2 }
  println("{p.x} {p.y}")
}
"#,
        "the result would lose the comment `-- a point`",
    );
    assert_fmt_refuses(
        "closure_close",
        r#"import list
fn main() {
  [1, 2] |> list.each { v -> -- explain v
    println("{v}")
  } -- done
}
"#,
        "the result would lose the comment `-- done`",
    );
}

/// The refusal points at the comment in the input.
#[test]
fn fmt_refusal_gives_the_position_of_the_lost_comment() {
    let ws = Workspace::new("position");
    ws.write(
        MAIN,
        "fn main() {\n  let x = -- why one\n    1\n  println(\"{x}\")\n}\n",
    );
    let fmt = ws.silt(&["fmt", MAIN]);
    assert_ended_cleanly("position", "silt fmt", &fmt);
    assert_eq!(fmt.code, Some(1), "{fmt:?}");
    assert!(
        fmt.stderr.contains("main.silt:2:11"),
        "the refusal must point at line 2, column 11\n{fmt:?}"
    );
}

/// One redundant pair of parentheses anywhere in the file used to switch
/// off the pass that keeps block comments inside expressions.
#[test]
fn fmt_refuses_when_a_block_comment_would_be_lost() {
    assert_fmt_refuses(
        "parens_block_comment",
        r#"fn add(a, b) = a + b
fn main() {
  let x = add(1, {- second -} 2)
  let y = (x + 1)
  println("{x} {y}")
}
"#,
        "the result would lose the comment `{- second -}`",
    );
}

/// One refused file does not stop the others from being formatted, and
/// the run as a whole fails.
#[test]
fn fmt_formats_the_other_files_when_one_is_refused() {
    let ws = Workspace::new("two_files");
    let refused = "fn main() {\n  let x = -- why one\n    1\n  println(\"{x}\")\n}\n";
    ws.write("refused.silt", refused);
    ws.write("fine.silt", "fn   main()  {\nprintln( \"hi\" )\n}\n");

    let fmt = ws.silt(&["fmt", "refused.silt", "fine.silt"]);
    assert_ended_cleanly("two_files", "silt fmt", &fmt);
    assert_eq!(fmt.code, Some(1), "{fmt:?}");
    assert_eq!(ws.read_bytes("refused.silt"), refused.as_bytes(), "{fmt:?}");
    assert_eq!(
        ws.read("fine.silt"),
        "fn main() {\n  println(\"hi\")\n}\n",
        "{fmt:?}"
    );
    assert!(
        fmt.stderr.contains("refused.silt") && !fmt.stderr.contains("fine.silt"),
        "only the refused file is reported\n{fmt:?}"
    );
}

/// Guard: the three answers of `--check` stay apart. 0 for a formatted
/// file, 1 with "not formatted" for one that would be reformatted.
/// (Status 2 for a refusal is asserted by `assert_fmt_refuses`.)
#[test]
fn fmt_check_still_tells_formatted_from_unformatted() {
    let ws = Workspace::new("check_states");
    ws.write(MAIN, "fn main() {\n  println(\"hi\")\n}\n");
    let formatted = ws.silt(&["fmt", "--check", MAIN]);
    assert_ended_cleanly("check_states", "silt fmt --check", &formatted);
    assert_eq!(formatted.code, Some(0), "{formatted:?}");

    let src = "fn   main()  {\nprintln( \"hi\" )\n}\n";
    ws.write(MAIN, src);
    let drift = ws.silt(&["fmt", "--check", MAIN]);
    assert_ended_cleanly("check_states", "silt fmt --check", &drift);
    assert_eq!(drift.code, Some(1), "{drift:?}");
    assert!(drift.stderr.contains("not formatted"), "{drift:?}");
    assert!(!drift.stderr.contains("formatting refused"), "{drift:?}");
    assert_eq!(ws.read_bytes(MAIN), src.as_bytes());
}

// ════════════════════════════════════════════════════════════════════
// 1b. Guards: what the formatter handles correctly still formats
// ════════════════════════════════════════════════════════════════════

/// `silt fmt` on `src` must succeed, the result must hold every text in
/// `fragments`, must be a fixed point of the formatter, and must run
/// with the same output as `src`.
fn assert_fmt_keeps_program(label: &str, src: &str, fragments: &[&str]) {
    let ws = Workspace::new(label);
    ws.write(MAIN, src);

    let before = ws.silt(&["run", MAIN]);
    assert_ended_cleanly(label, "silt run (input)", &before);
    assert_eq!(
        before.code,
        Some(0),
        "{label}: the input must run\n{before:?}"
    );

    let fmt = ws.silt(&["fmt", MAIN]);
    assert_ended_cleanly(label, "silt fmt", &fmt);
    assert_eq!(
        fmt.code,
        Some(0),
        "{label}: `silt fmt` must succeed\n{fmt:?}"
    );
    assert!(
        fmt.stderr.is_empty(),
        "{label}: `silt fmt` must print nothing\n{fmt:?}"
    );
    let formatted = ws.read(MAIN);
    for fragment in fragments {
        assert!(
            formatted.contains(fragment),
            "{label}: the result must hold `{fragment}`\n--- result ---\n{formatted}"
        );
    }

    let check = ws.silt(&["fmt", "--check", MAIN]);
    assert_ended_cleanly(label, "silt fmt --check", &check);
    assert_eq!(
        check.code,
        Some(0),
        "{label}: the result must count as formatted\n{check:?}\n--- result ---\n{formatted}"
    );
    let again = ws.silt(&["fmt", MAIN]);
    assert_ended_cleanly(label, "silt fmt (second pass)", &again);
    assert_eq!(again.code, Some(0), "{label}: second pass\n{again:?}");
    assert_eq!(
        ws.read(MAIN),
        formatted,
        "{label}: a second pass must not change the result"
    );

    let after = ws.silt(&["run", MAIN]);
    assert_ended_cleanly(label, "silt run (result)", &after);
    assert_eq!(
        after.code,
        Some(0),
        "{label}: the result must run\n{after:?}"
    );
    assert_eq!(
        after.stdout, before.stdout,
        "{label}: the result must print what the input printed\n--- result ---\n{formatted}"
    );
}

/// Guard. A lambda in last-argument position becomes a trailing closure;
/// its body `{ e }` becomes the expression `e`, and its parameter `_`
/// becomes the wildcard pattern.
#[test]
fn fmt_still_turns_a_last_argument_lambda_into_a_trailing_closure() {
    assert_fmt_keeps_program(
        "trailing_closure",
        r#"import list
fn main() {
  let ys = list.map([1, 2, 3], fn(x) { x * 2 })
  let zs = list.filter(ys, fn(_) { true })
  let n = list.fold(zs, 0, fn(acc, x) {
    let next = acc + x
    next
  })
  println("{ys} {zs} {n}")
}
"#,
        &[
            "list.map([1, 2, 3]) { x -> x * 2 }",
            "list.filter(ys) { _ -> true }",
            "list.fold(zs, 0) { acc, x ->",
        ],
    );
}

/// Guard. A closure that is not the last argument becomes `fn(...) { }`;
/// its body `e` becomes the block `{ e }`.
#[test]
fn fmt_still_turns_a_closure_elsewhere_into_a_fn_lambda() {
    assert_fmt_keeps_program(
        "closure_not_last",
        r#"fn apply(f, x) = f(x)
fn main() {
  let a = apply({ x -> x + 1 }, 1)
  let b = apply({ _ -> 7 }, 1)
  let c = apply(fn(x) { x * 3 }, 2)
  println("{a} {b} {c}")
}
"#,
        &["apply(fn(x) {", "apply(fn(_) {"],
    );
}

/// Guard. Redundant parentheses go, needed ones stay.
#[test]
fn fmt_still_drops_redundant_parentheses() {
    assert_fmt_keeps_program(
        "parens",
        r#"fn main() {
  let a = 2
  let b = 3
  let c = ((a + b)) * (a)
  let d = (a * b) + (c)
  let e = -(a + b)
  let f = (a)
  println("{c} {d} {e} {f}")
}
"#,
        &[
            "let c = (a + b) * a",
            "let d = a * b + c",
            "let e = -(a + b)",
            "let f = a\n",
        ],
    );
}

/// Guard. Number literals are respelled in decimal, a line break in a
/// string becomes `\n`, a triple-quoted string is kept as written.
#[test]
fn fmt_still_respells_literals() {
    assert_fmt_keeps_program(
        "literals",
        "fn main() {\n  let a = 0xFF\n  let b = 0b1010\n  let c = 1_000_000\n  let d = 1e3\n  \
         let e = 2.50\n  let s = \"line one\nline two\"\n  let t = \"\"\"\n    raw { not \
         interpolated }\n      indented\n    \"\"\"\n  println(\"{a} {b} {c} {d} {e}\")\n  \
         println(s)\n  println(t)\n}\n",
        &[
            "let a = 255",
            "let b = 10",
            "let c = 1000000",
            "let d = 1000.0",
            "let e = 2.5",
            "let s = \"line one\\nline two\"",
            "    raw { not interpolated }\n      indented\n    \"\"\"",
        ],
    );
}

/// Guard. Trailing commas are kept where the input has them.
#[test]
fn fmt_still_keeps_trailing_commas() {
    assert_fmt_keeps_program(
        "trailing_commas",
        r#"type P {
  x: Int,
  y: Int,
}
fn add(a, b,) = a + b
fn main() {
  let xs = [1, 2, 3,]
  let t = (1, 2,)
  let p = P { x: 1, y: 2, }
  let m = #{ "a": 1, "b": 2, }
  let n = add(1, 2,)
  let r = match n {
    3 -> "three",
    _ -> "other",
  }
  println("{xs} {t} {p.x} {m} {n} {r}")
}
"#,
        &[
            "fn add(a, b,) = a + b",
            "let xs = [1, 2, 3,]",
            "let n = add(1, 2,)",
            "3 -> \"three\",",
        ],
    );
}

/// Guard. `where` bounds on one type variable are gathered.
#[test]
fn fmt_still_groups_where_bounds_by_type_variable() {
    assert_fmt_keeps_program(
        "where_grouping",
        r#"fn show_both(x: a, y: b) -> String where a: Display, b: Display, a: Compare {
  "{x} {y}"
}
fn main() {
  println(show_both(1, "two"))
}
"#,
        &["where a: Display + Compare, b: Display {"],
    );
}

/// Guard. Imports are sorted; the comments above and behind an import
/// go with it, and no comment is lost.
#[test]
fn fmt_still_sorts_imports_and_keeps_their_comments() {
    assert_fmt_keeps_program(
        "import_sort",
        r#"-- the program header
import string
-- lists come second here
import list
import option -- trailing on an import

-- before main
fn main() {
  let xs = [3, 1, 2] |> list.sort
  let s = string.join(list.map(xs) { x -> "{x}" }, ",")
  let o = option.unwrap_or(list.head(xs), 0)
  println("{s} {o}") -- trailing on a statement
  -- last comment in main
}
-- after everything
"#,
        &[
            "import list\nimport option -- trailing on an import\nimport string\n",
            "-- the program header",
            "-- lists come second here",
            "-- before main",
            "println(\"{s} {o}\") -- trailing on a statement",
            "-- last comment in main",
            "-- after everything",
        ],
    );
}

/// Guard. Comments in the places where the printer looks for them.
#[test]
fn fmt_still_keeps_comments_in_ordinary_places() {
    assert_fmt_keeps_program(
        "comments",
        r#"-- leading file comment
type Shape {
  Circle(Float), -- round
  Square(Float), -- boxy
}

{- a block comment
   over two lines -}
fn area(s) {
  -- pick the formula
  match s {
    Circle(r) -> 3.0 * r * r -- roughly
    -- the easy one
    Square(w) -> w * w
  }
}

fn main() {
  let shapes = [
    Circle(1.0), -- first
    Square(2.0), -- second
  ]
  let total = area(Circle(1.0)) {- inline -} + area(Square(2.0))
  println("{total}") -- done
}
"#,
        &[
            "-- leading file comment",
            "Circle(Float), -- round",
            "Square(Float), -- boxy",
            "{- a block comment\n   over two lines -}",
            "  -- pick the formula",
            "Circle(r) -> 3.0 * r * r -- roughly",
            "    -- the easy one",
            "Circle(1.0), -- first",
            "Square(2.0), -- second",
            "area(Circle(1.0)) {- inline -} + area(Square(2.0))",
            "println(\"{total}\") -- done",
        ],
    );
}

/// Guard. A program that uses most of the language, without comments.
#[test]
fn fmt_still_formats_an_ordinary_program() {
    assert_fmt_keeps_program(
        "ordinary",
        r#"import list
import string

type User {
  name: String,
  age: Int,
}

trait Greet {
  fn greet(self) -> String
}

trait Greet for User {
  fn greet(self) -> String = "hi {self.name}"
}

fn classify(n) {
  match {
    n < 0 -> "negative"
    n == 0 -> "zero"
    _ -> "positive"
  }
}

fn sum_to(n) {
  loop i = n, acc = 0 {
    match i {
      0 -> acc
      _ -> loop(i - 1, acc + i)
    }
  }
}

fn main() {
  let u = User { name: "Ann", age: 30 }
  let older = u.{ age: 31 }
  when let Some(first) = list.head([1, 2, 3]) else {
    return
  }
  when older.age > 30 else {
    return
  }
  let nested = list.map([1], fn(x) { match x {
    1 -> "one"
    _ -> "other"
  } })
  let words = "a b c" |> string.split(" ") |> list.map { w -> string.to_upper(w) }
  println(u.greet())
  println("{classify(first)} {sum_to(4)} {words} {older.age} {nested}")
}
"#,
        &[
            "  |> string.split(\" \")\n  |> list.map { w -> string.to_upper(w) }",
            "let nested = list.map([1]) { x -> match x {",
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// 2. Expression height
// ════════════════════════════════════════════════════════════════════

/// Number of links in the chains that must be rejected.
const TOO_LONG: usize = 10_000;

/// Number of links in the chains that must keep working.
const ORDINARY: usize = 200;

fn plus_chain(links: usize) -> String {
    let terms = vec!["1"; links].join(" + ");
    format!("fn main() {{\n  let x = {terms}\n  println(\"{{x}}\")\n}}\n")
}

fn pipe_chain(links: usize) -> String {
    let stages = " |> id".repeat(links);
    format!("fn id(x) = x\nfn main() {{\n  let x = 1{stages}\n  println(\"{{x}}\")\n}}\n")
}

fn call_chain(links: usize) -> String {
    let calls = "()".repeat(links);
    format!("fn f() = f\nfn main() {{\n  let x = f{calls}\n  println(\"done\")\n}}\n")
}

fn field_chain(links: usize) -> String {
    let fields = ".me".repeat(links);
    format!(
        "type R {{ me: R, n: Int }}\nfn last(r: R) -> Int = r{fields}.n\nfn main() {{\n  \
         println(\"ok\")\n}}\n"
    )
}

/// `silt <subcommand>` on a chain that is too long must fail with a
/// parse error that has a position, and must not overflow the stack.
fn assert_too_deep(label: &str, src: &str) {
    let ws = Workspace::new(label);
    ws.write(MAIN, src);
    for subcommand in ["check", "run", "fmt"] {
        let what = format!("silt {subcommand}");
        let out = ws.silt(&[subcommand, MAIN]);
        assert_ended_cleanly(label, &what, &out);
        assert_eq!(out.code, Some(1), "{label}: `{what}` must fail\n{out:?}");
        assert!(
            out.stderr.contains("error[parse]") && out.stderr.contains("expression is too deep"),
            "{label}: `{what}` must report the expression as too deep\n{out:?}"
        );
        assert!(
            out.stderr.contains("main.silt:"),
            "{label}: `{what}` must give a position\n{out:?}"
        );
        assert_eq!(
            ws.read_bytes(MAIN),
            src.as_bytes(),
            "{label}: `{what}` must leave the file untouched"
        );
    }
}

#[test]
fn an_operator_chain_of_10000_terms_is_a_parse_error() {
    assert_too_deep("plus_too_long", &plus_chain(TOO_LONG));
}

#[test]
fn a_pipe_chain_of_10000_stages_is_a_parse_error() {
    assert_too_deep("pipe_too_long", &pipe_chain(TOO_LONG));
}

#[test]
fn a_call_chain_of_10000_calls_is_a_parse_error() {
    assert_too_deep("call_too_long", &call_chain(TOO_LONG));
}

#[test]
fn a_field_access_chain_of_10000_accesses_is_a_parse_error() {
    assert_too_deep("field_too_long", &field_chain(TOO_LONG));
}

/// The limit is on the tree, not on one chain: 40 chains of 300 links,
/// each with the one before as its first operand, are 12,000 levels.
#[test]
fn chains_stacked_on_one_another_count_towards_one_height() {
    // ((((1) + 1 + ... + 1) * 1 * ... * 1) + 1 ...), 300 operators per level.
    let mut src = String::from("fn main() {\n  let x = ");
    src.push_str(&"(".repeat(40));
    src.push('1');
    for level in 0..40 {
        src.push(')');
        src.push_str(&(if level % 2 == 0 { " + 1" } else { " * 1" }).repeat(300));
    }
    src.push_str("\n  println(\"{x}\")\n}\n");
    assert_too_deep("stacked_chains", &src);
}

/// Guard. Chains of 200 links of every kind keep working.
#[test]
fn chains_of_200_links_still_work() {
    let label = "ordinary_chains";

    let ws = Workspace::new(label);
    ws.write(MAIN, &plus_chain(ORDINARY));
    let plus = ws.silt(&["run", MAIN]);
    assert_ended_cleanly(label, "silt run (operators)", &plus);
    assert_eq!(plus.code, Some(0), "{plus:?}");
    assert_eq!(plus.stdout.trim(), "200", "{plus:?}");

    ws.write(MAIN, &pipe_chain(ORDINARY));
    let pipe = ws.silt(&["run", MAIN]);
    assert_ended_cleanly(label, "silt run (pipes)", &pipe);
    assert_eq!(pipe.code, Some(0), "{pipe:?}");
    assert_eq!(pipe.stdout.trim(), "1", "{pipe:?}");

    ws.write(MAIN, &field_chain(ORDINARY));
    let field = ws.silt(&["check", MAIN]);
    assert_ended_cleanly(label, "silt check (field accesses)", &field);
    assert_eq!(field.code, Some(0), "{field:?}");

    // Calls and field accesses in turn, 200 of each.
    let links = ".next()".repeat(ORDINARY);
    ws.write(
        MAIN,
        &format!(
            "type R {{ next: Fn() -> R, n: Int }}\n\
             fn mk(n: Int) -> R = R {{ next: fn() {{ mk(n + 1) }}, n: n }}\n\
             fn main() {{\n  let r = mk(0){links}\n  println(\"{{r.n}}\")\n}}\n"
        ),
    );
    let calls = ws.silt(&["run", MAIN]);
    assert_ended_cleanly(label, "silt run (calls)", &calls);
    assert_eq!(calls.code, Some(0), "{calls:?}");
    assert_eq!(calls.stdout.trim(), "200", "{calls:?}");

    // `f()()...` has no type, in a chain of any length. It must still get
    // as far as the type checker.
    ws.write(MAIN, &call_chain(ORDINARY));
    let untyped = ws.silt(&["check", MAIN]);
    assert_ended_cleanly(label, "silt check (calls only)", &untyped);
    assert!(
        untyped.stderr.contains("error[type]") && !untyped.stderr.contains("error[parse]"),
        "{untyped:?}"
    );
}

/// Guard. The formatter handles a long chain, too.
#[test]
fn a_chain_of_200_links_still_formats() {
    assert_fmt_keeps_program("format_chain", &plus_chain(ORDINARY), &["1 + 1 + 1"]);
    assert_fmt_keeps_program("format_pipes", &pipe_chain(40), &["  |> id\n  |> id\n"]);
}

// ════════════════════════════════════════════════════════════════════
// 3. Expressions that are followed by a block
// ════════════════════════════════════════════════════════════════════

const LIST_TYPE: &str = "type L { Nil, Cons(Int, L) }\n";

/// Whether the `{` of the loop body is on the line of the header or on
/// the next one makes no difference, to the parser or to the formatter
/// (which always puts it on the line of the header).
#[test]
fn a_loop_header_means_the_same_with_the_brace_on_either_line() {
    let body = r#"    match i {
      0 -> acc
      _ -> loop(i - 1, Cons(i, acc))
    }
  }
}
fn main() {
  println("{build(3)}")
}
"#;
    let same_line = format!("{LIST_TYPE}fn build(n) {{\n  loop i = n, acc = Nil {{\n{body}");
    let next_line = format!("{LIST_TYPE}fn build(n) {{\n  loop i = n, acc = Nil\n  {{\n{body}");
    assert_fmt_keeps_program(
        "fmt_brace_same_line",
        &same_line,
        &["loop i = n, acc = Nil {"],
    );
    assert_fmt_keeps_program(
        "fmt_brace_next_line",
        &next_line,
        &["loop i = n, acc = Nil {"],
    );
}

#[test]
fn a_trailing_closure_works_inside_call_arguments_in_a_scrutinee() {
    let src = r#"import list
fn main() {
  let xs = [1, 2, 3]
  let r = match list.length(list.filter(xs) { x -> x > 1 }) {
    0 -> "none"
    _ -> "some"
  }
  println(r)
}
"#;
    assert_fmt_keeps_program(
        "fmt_scrutinee_call_arg",
        src,
        &["match list.length(list.filter(xs) { x -> x > 1 }) {"],
    );
}

#[test]
fn a_match_body_may_follow_the_right_operand_of_a_pipe() {
    let head = r#"import list
fn main() {
  let xs = [1, 2]
  let r = match xs |> list.head {
    Some(x) -> x
    None -> 0
  }
  println("{r}")
}
"#;
    assert_fmt_keeps_program("fmt_pipe_head", head, &["  |> list.head {\n"]);
}

/// Guard. A trailing closure on the right operand of a pipe in a
/// scrutinee, followed by the match body, is still a closure.
#[test]
fn a_closure_in_a_piped_scrutinee_is_still_a_closure() {
    let src = r#"import list
fn main() {
  let items = [1, 7, 3]
  let r = match items |> list.any { x -> x > 5 } {
    true -> "big"
    false -> "small"
  }
  let s = match items |> list.filter { x -> x > 1 } |> list.map { x -> x * 2 } {
    [] -> "none"
    ys -> "{ys}"
  }
  let t = match items
    |> list.filter { x -> x > 1 }
    |> list.length
  {
    2 -> "two"
    _ -> "other"
  }
  println("{r} {s} {t}")
}
"#;
    assert_fmt_keeps_program(
        "fmt_pipe_closure_then_body",
        src,
        &["  |> list.any { x -> x > 5 } {\n"],
    );
}
