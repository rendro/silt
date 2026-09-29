//! Typechecker regression tests for four defects.
//!
//! T1. A refutable pattern was accepted in a closure parameter. The
//!     compiler emits no test for a parameter pattern, so
//!     `{ Dollars(n) -> n * 100 }` read the payload of `Cents(250)` as
//!     dollars, and `{ Some(x) -> x + 1 }` failed at run time on `None`.
//!     Irrefutability is now one judgement, answered by the
//!     exhaustiveness checker and asked at every binding site that has
//!     no failure branch.
//!
//! T2. Every `module.function(...)` call was accepted with one argument
//!     missing. The tolerance existed for five builtins whose last
//!     argument is optional. A signature now states whether its last
//!     parameter is optional; every other call needs the exact number of
//!     arguments, and the pipe form follows the same rule as the call
//!     form.
//!
//! T3. A `match` whose arms all diverge was not typed `Never`, so it
//!     could not be the `else` body of a `when let`.
//!
//! T4. A type error in the body of a trait impl written against an alias
//!     of the target type (or against `Range`, or `Fun`) was dropped
//!     whenever any function scheme was narrowed.
//!
//! Every test runs the built `silt` binary on files in a fresh temporary
//! directory and asserts on its exit status and output. Each run has a
//! timeout, so a hang fails the test instead of hanging the suite.
//!
//! A test marked GUARD pins behaviour that must not change: it passes
//! before and after the fix. Every other test fails before the fix.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Upper bound for one run of the binary. A run that exceeds it is
/// killed and reported as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

/// What every refutable-pattern diagnostic must tell the user to do.
const ADVICE: [&str; 2] = ["`match`", "`when let ... else`"];

#[derive(Debug)]
struct Outcome {
    /// Exit status; `None` if the process was ended by a signal.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// True if the run exceeded `RUN_TIMEOUT` and was killed.
    timed_out: bool,
}

impl Outcome {
    /// Everything the run printed. Diagnostics are asserted on this, so
    /// a test does not depend on which stream carries them.
    fn output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// True if the run finished in time with a failure status.
    fn failed(&self) -> bool {
        !self.timed_out && self.code.is_some() && self.code != Some(0)
    }

    /// True if some error of the run shows the source line `statement`
    /// and contains `message`. An error is the text from one `error[` to
    /// the next diagnostic; it quotes the line it points at.
    fn has_diagnostic(&self, statement: &str, message: &str) -> bool {
        self.output().split("error[").skip(1).any(|rest| {
            let error = rest.split("warning[").next().unwrap_or(rest);
            error.contains(statement) && error.contains(message)
        })
    }
}

fn fresh_dir(label: &str) -> PathBuf {
    static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let unique = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    let name = format!("silt_wave1_typechecker_{pid}_{unique}_{label}");
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

    let outcome = Outcome {
        code: status.code(),
        stdout: read_text(&out_path),
        stderr: read_text(&err_path),
        timed_out,
    };
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

fn run_program(label: &str, subcommand: &str, src: &str) -> Outcome {
    run_silt(label, subcommand, &[("main.silt", src)])
}

/// `silt run` must finish in time, exit with status 0 and print exactly
/// `expected`.
fn assert_runs_files(label: &str, files: &[(&str, &str)], expected: &str) {
    let outcome = run_silt(label, "run", files);
    assert!(
        !outcome.timed_out,
        "[{label}] `silt run` did not finish within {RUN_TIMEOUT:?}: {outcome:?}"
    );
    assert_eq!(
        outcome.code,
        Some(0),
        "[{label}] `silt run` must succeed: {outcome:?}"
    );
    assert_eq!(
        outcome.stdout, expected,
        "[{label}] unexpected output: {outcome:?}"
    );
}

fn assert_runs(label: &str, src: &str, expected: &str) {
    assert_runs_files(label, &[("main.silt", src)], expected);
}

/// `silt check` must finish in time, fail, and print a diagnostic that
/// contains every one of `needles`. No error may come from the VM.
/// Returns the outcome for further assertions.
fn assert_check_rejects_files(label: &str, files: &[(&str, &str)], needles: &[&str]) -> Outcome {
    let outcome = run_silt(label, "check", files);
    assert!(
        outcome.failed(),
        "[{label}] `silt check` must finish and reject the program: {outcome:?}"
    );
    let output = outcome.output();
    for needle in needles {
        assert!(
            output.contains(needle),
            "[{label}] the diagnostic must contain {needle:?}: {outcome:?}"
        );
    }
    assert!(
        !output.contains("error[runtime]"),
        "[{label}] the error must come from the checker: {outcome:?}"
    );
    outcome
}

fn assert_check_rejects(label: &str, src: &str, needles: &[&str]) -> Outcome {
    assert_check_rejects_files(label, &[("main.silt", src)], needles)
}

/// `silt run` must refuse a program that does not type check: it fails,
/// reports `needle`, reports no run-time error, and prints nothing of
/// what the program would print.
fn assert_run_refuses(label: &str, src: &str, needle: &str) {
    let outcome = run_program(label, "run", src);
    assert!(
        outcome.failed(),
        "[{label}] `silt run` must finish and refuse the program: {outcome:?}"
    );
    let output = outcome.output();
    assert!(
        output.contains(needle),
        "[{label}] `silt run` must report {needle:?}: {outcome:?}"
    );
    assert!(
        !output.contains("error[runtime]"),
        "[{label}] the program must not reach the VM: {outcome:?}"
    );
    assert_eq!(
        outcome.stdout, "",
        "[{label}] the program must not run: {outcome:?}"
    );
}

/// The needles of a refutable-pattern diagnostic: the site, the reason
/// and what to write instead.
fn refutable_needles<'a>(site: &'a str, reason: &'a str) -> Vec<&'a str> {
    let mut needles = vec![site, reason];
    needles.extend(ADVICE);
    needles
}

const CLOSURE_PARAMETER: &str = "refutable pattern in closure parameter";
const LET: &str = "refutable pattern in `let`";

// ── T1: irrefutability at every binding site ─────────────────────────

/// The silent wrong result: `Cents(250)` was read as `Dollars(250)`
/// and the program printed `[500, 25000]`.
#[test]
fn t1_closure_parameter_rejects_constructor_of_multi_variant_enum() {
    let src = r#"
import list
type Money { Dollars(Int), Cents(Int) }
fn main() {
  println([Dollars(5), Cents(250)] |> list.map { Dollars(n) -> n * 100 })
}
"#;
    assert_check_rejects(
        "closure_money",
        src,
        &refutable_needles(
            CLOSURE_PARAMETER,
            "constructor 'Dollars' is only one of 2 variants of enum 'Money'",
        ),
    );
    assert_run_refuses("closure_money_run", src, CLOSURE_PARAMETER);
}

/// The run-time failure: `variant destructure: field index 0 out of
/// bounds` on `None`.
#[test]
fn t1_closure_parameter_rejects_option_constructor() {
    let src = r#"
import list
fn main() {
  println([Some(1), None] |> list.map { Some(x) -> x + 1 })
}
"#;
    assert_check_rejects(
        "closure_option",
        src,
        &refutable_needles(
            CLOSURE_PARAMETER,
            "constructor 'Some' is only one of 2 variants of enum 'Option'",
        ),
    );
    assert_run_refuses("closure_option_run", src, CLOSURE_PARAMETER);
}

/// The refutable part may sit inside an irrefutable tuple pattern.
#[test]
fn t1_closure_parameter_rejects_refutable_part_nested_in_a_tuple() {
    assert_check_rejects(
        "closure_nested",
        r#"
import list
fn main() {
  println([(1, Some(2)), (3, None)] |> list.map { (a, Some(b)) -> a + b })
}
"#,
        &refutable_needles(
            CLOSURE_PARAMETER,
            "constructor 'Some' is only one of 2 variants of enum 'Option'",
        ),
    );
}

/// Every parameter of a closure is a binding site, not only the first.
#[test]
fn t1_closure_parameter_rejects_refutable_second_parameter() {
    assert_check_rejects(
        "closure_second_param",
        r#"
import list
fn main() {
  println(list.fold([Some(1), None], 0) { acc, Some(x) -> acc + x })
}
"#,
        &refutable_needles(
            CLOSURE_PARAMETER,
            "constructor 'Some' is only one of 2 variants of enum 'Option'",
        ),
    );
}

/// GUARD. Irrefutable destructuring in closure parameters keeps working:
/// tuples, nested tuples, single-variant constructors (plain, generic,
/// nested in a tuple), wildcards, and plain names in both closure forms.
#[test]
fn t1_guard_irrefutable_closure_parameters_are_accepted() {
    assert_runs(
        "closure_guards",
        r#"
import list
import map

type Wrap { W(Int) }
type Box(a) { Boxed(a) }

fn main() {
  println([(1, "a"), (2, "b")] |> list.map { (n, s) -> "{n}{s}" })
  println([((1, 2), 3)] |> list.map { ((a, b), c) -> a + b + c })
  println([W(1), W(2)] |> list.map { W(n) -> n + 1 })
  println([Boxed("p"), Boxed("q")] |> list.map { Boxed(s) -> s })
  println([(W(1), 10)] |> list.map { (W(n), m) -> n + m })
  println([(1, 2)] |> list.map { (_, b) -> b })
  println(list.fold([(1, 2), (3, 4)], 0) { acc, (a, b) -> acc + a + b })
  println(#{ "k": 1 } |> map.entries |> list.map { (k, v) -> "{k}={v}" })
  println([1, 2] |> list.map { x -> x * 2 })
  println([1, 2] |> list.map(fn(x) { x * 3 }))
}
"#,
        "[1a, 2b]\n[6]\n[2, 3]\n[p, q]\n[11]\n[2]\n10\n[k=1]\n[2, 4]\n[3, 6]\n",
    );
}

/// GUARD. Irrefutable destructuring in `let` keeps working: tuples,
/// nested tuples, single-variant constructors at any depth, records
/// (named, generic, anonymous, nested in a tuple), wildcards, the unit
/// pattern and the list pattern that takes any list.
#[test]
fn t1_guard_irrefutable_let_patterns_are_accepted() {
    assert_runs(
        "let_guards",
        r#"
type Wrap { W(Int) }
type Pt { x: Int, y: Int }
type Box(a) { Boxed(a) }
type Pair(a, b) { first: a, second: b }

fn main() {
  let (a, b) = (1, "s")
  println("{a}{b}")
  let ((c, d), e) = ((1, 2), 3)
  println(c + d + e)
  let W(n) = W(5)
  println(n)
  let Boxed(s) = Boxed("p")
  println(s)
  let (W(m), k) = (W(1), 10)
  println(m + k)
  let ((W(q), r), t) = ((W(1), 2), 3)
  println(q + r + t)
  let Pt { x, y } = Pt { x: 1, y: 2 }
  println(x + y)
  let Pt { x: px, y: _ } = Pt { x: 7, y: 2 }
  println(px)
  let Pair { first, second } = Pair { first: 1, second: "z" }
  println("{first}{second}")
  let (Pt { x: x2, y: y2 }, z2) = (Pt { x: 1, y: 2 }, 3)
  println(x2 + y2 + z2)
  let (_, w) = (1, 2)
  println(w)
  let () = ()
  let _ = 5
  let {name, age} = {name: "A", age: 3}
  println("{name}{age}")
  let [..rest] = [1, 2, 3]
  println(rest)
  let ([..rest2], u) = ([1, 2], 3)
  println(rest2)
  println(u)
}
"#,
        "1s\n6\n5\np\n11\n6\n3\n7\n1z\n6\n2\nA3\n[1, 2, 3]\n[1, 2]\n3\n",
    );
}

/// GUARD. `let` rejects every refutable shape. Each statement must have
/// its own diagnostic, with the site, the reason and the advice.
#[test]
fn t1_guard_refutable_let_patterns_are_rejected() {
    let cases: [(&str, &str); 9] = [
        (
            "let Dollars(n) = v",
            "constructor 'Dollars' is only one of 2 variants of enum 'Money'",
        ),
        (
            "let (a, Some(b)) = (1, Some(2))",
            "constructor 'Some' is only one of 2 variants of enum 'Option'",
        ),
        ("let [x, y] = [1, 2]", "list patterns can fail to match"),
        ("let 5 = 5", "integer literal patterns"),
        ("let true = false", "boolean literal patterns"),
        ("let 1..10 = 999", "range patterns"),
        ("let #{ \"k\": kv } = #{ \"k\": 1 }", "map patterns"),
        ("let ^pinned = 99", "pin patterns"),
        (
            "let Pt { x: 0, y: py } = Pt { x: 1, y: 2 }",
            "integer literal patterns",
        ),
    ];
    let mut src = String::from(
        "type Money { Dollars(Int), Cents(Int) }\n\
         type Pt { x: Int, y: Int }\n\n\
         fn main() {\n  let v = Cents(250)\n  let pinned = 5\n",
    );
    for (statement, _) in cases {
        src.push_str(&format!("  {statement}\n"));
    }
    src.push_str("  println(\"bound\")\n}\n");

    let outcome = assert_check_rejects("let_refutable", &src, &[LET]);
    for (statement, reason) in cases {
        for needle in refutable_needles(LET, reason) {
            assert!(
                outcome.has_diagnostic(statement, needle),
                "`{statement}` must be rejected with {needle:?}: {outcome:?}"
            );
        }
    }
}

/// GUARD. A pin pattern tests for one value, also on a value of an
/// opaque builtin type, which has no constructors to enumerate.
#[test]
fn t1_guard_let_rejects_pin_on_opaque_type() {
    assert_check_rejects(
        "let_pin_opaque",
        r#"
import bytes

fn main() {
  let a = bytes.from_string("x")
  let b = bytes.from_string("y")
  let ^a = b
  println("bound")
}
"#,
        &refutable_needles(LET, "pin patterns"),
    );
}

/// An or-pattern whose alternatives cover every variant is irrefutable:
/// it alone is exhaustive. It was rejected once per alternative.
#[test]
fn t1_let_accepts_or_pattern_that_covers_every_variant() {
    assert_runs(
        "let_or_covering",
        r#"
type Money { Dollars(Int), Cents(Int) }
fn amount(v: Money) -> Int {
  let Dollars(n) | Cents(n) = v
  n
}
fn main() {
  println(amount(Dollars(5)))
  println(amount(Cents(250)))
}
"#,
        "5\n250\n",
    );
}

/// GUARD. An or-pattern that leaves a variant out stays refutable.
#[test]
fn t1_guard_let_rejects_or_pattern_that_misses_a_variant() {
    assert_check_rejects(
        "let_or_partial",
        r#"
type Coin { Penny(Int), Dime(Int), Note(Int) }
fn main() {
  let v = Note(5)
  let Penny(n) | Dime(n) = v
  println(n)
}
"#,
        &refutable_needles(LET, "of enum 'Coin'"),
    );
}

/// A record type with 24 fields, all `Bool` except the last.
const WIDE_RECORD: &str = r#"
type Cfg {
  f01: Bool, f02: Bool, f03: Bool, f04: Bool, f05: Bool, f06: Bool,
  f07: Bool, f08: Bool, f09: Bool, f10: Bool, f11: Bool, f12: Bool,
  f13: Bool, f14: Bool, f15: Bool, f16: Bool, f17: Bool, f18: Bool,
  f19: Bool, f20: Bool, f21: Bool, f22: Bool, f23: Bool, f24: Option(Int),
}

fn mk() -> Cfg {
  Cfg {
    f01: true, f02: true, f03: true, f04: true, f05: true, f06: true,
    f07: true, f08: true, f09: true, f10: true, f11: true, f12: true,
    f13: true, f14: true, f15: true, f16: true, f17: true, f18: true,
    f19: true, f20: true, f21: true, f22: true, f23: true, f24: Some(1),
  }
}
"#;

/// GUARD. Wide product patterns are judged in time: a record with 24
/// fields and a tuple with 22 elements, bound by plain names.
#[test]
fn t1_guard_wide_let_patterns_are_accepted() {
    let src = format!(
        "{WIDE_RECORD}\n\
         fn main() {{\n  \
           let Cfg {{ f01, f23 }} = mk()\n  \
           println(\"{{f01}}{{f23}}\")\n  \
           let (a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15, a16, a17, \
                a18, a19, a20, a21, a22) =\n    \
             (true, true, true, true, true, true, true, true, true, true, true, true, true, \
              true, true, true, true, true, true, true, true, true)\n  \
           println(\"{{a1}}{{a22}}\")\n\
         }}\n"
    );
    assert_runs("let_wide", &src, "truetrue\ntruetrue\n");
}

/// GUARD. A refutable part behind 23 other fields is found, in time,
/// and named.
#[test]
fn t1_guard_wide_let_pattern_with_refutable_field_is_rejected() {
    let src = format!(
        "{WIDE_RECORD}\n\
         fn main() {{\n  \
           let Cfg {{ f24: Some(n), f01 }} = mk()\n  \
           println(\"{{f01}}{{n}}\")\n\
         }}\n"
    );
    assert_check_rejects(
        "let_wide_refutable",
        &src,
        &refutable_needles(
            LET,
            "constructor 'Some' is only one of 2 variants of enum 'Option'",
        ),
    );
}

/// A `match` on the 24-field record with one covering arm is
/// exhaustive, and is decided in time. The checker used to enumerate
/// both constructors of every `Bool` field and did not finish.
#[test]
fn t1_match_on_wide_record_is_decided_in_time() {
    let src = format!(
        "{WIDE_RECORD}\n\
         fn main() {{\n  \
           match mk() {{\n    \
             Cfg {{ f01, f23 }} -> println(\"{{f01}}{{f23}}\")\n  \
           }}\n\
         }}\n"
    );
    assert_runs("match_wide", &src, "truetrue\n");
}

/// The judgement is the exhaustiveness checker's, so `match` answers
/// the same way as `let`: a single arm that destructures single-variant
/// constructors, or takes any list, inside a tuple is exhaustive. Both
/// matches were reported non-exhaustive.
#[test]
fn t1_match_accepts_arm_that_covers_by_nested_irrefutable_parts() {
    assert_runs(
        "match_nested_covering",
        r#"
type Wrap { W(Int) }

fn main() {
  let v = ((W(1), 2), 3)
  match v {
    ((W(q), r), t) -> println(q + r + t)
  }
  let lv = ([1, 2], 3)
  match lv {
    ([..rest], u) -> println("{rest}{u}")
  }
}
"#,
        "6\n[1, 2]3\n",
    );
}

/// A `match` on a value of an opaque builtin type whose only arm is a
/// pin pattern is not exhaustive. It passed `silt check` and failed at
/// run time with "no arm matched".
#[test]
fn t1_match_with_only_a_pin_arm_on_opaque_type_is_not_exhaustive() {
    assert_check_rejects(
        "match_pin_opaque",
        r#"
import bytes

fn main() {
  let a = bytes.from_string("x")
  let b = bytes.from_string("y")
  let r = match b {
    ^a -> "same"
  }
  println(r)
}
"#,
        &["non-exhaustive match"],
    );
}

/// GUARD. A column whose declared type is an alias is judged by the
/// alias's target: these matches are exhaustive.
#[test]
fn t1_guard_match_on_alias_typed_column_is_exhaustive() {
    assert_runs(
        "match_alias_column",
        r#"
type Flag = Bool
type W { A(Flag), B }
type R { on: Flag, n: Int }

fn name(w: W) -> String {
  match w {
    A(true) -> "a-true"
    A(false) -> "a-false"
    B -> "b"
  }
}

fn rec(r: R) -> String {
  match r {
    R { on: true, n } -> "on {n}"
    R { on: false, n } -> "off {n}"
  }
}

fn main() {
  println(name(A(true)))
  println(name(A(false)))
  println(name(B))
  println(rec(R { on: true, n: 1 }))
  println(rec(R { on: false, n: 2 }))
}
"#,
        "a-true\na-false\nb\non 1\noff 2\n",
    );
}

// ── T2: call arity ───────────────────────────────────────────────────

/// One accepted way to call a builtin whose last argument is optional.
struct OptionalCall {
    /// The builtin.
    builtin: &'static str,
    /// "call" for `f(a, b)`, "pipe" for `a |> f(b)` or `a |> f`.
    form: &'static str,
    /// Whether the optional argument is passed.
    optional_passed: bool,
    /// The statement, one line in the body of `main`.
    statement: &'static str,
    /// What the statement prints, if anything.
    prints: &'static str,
}

const fn optional_call(
    builtin: &'static str,
    form: &'static str,
    optional_passed: bool,
    statement: &'static str,
    prints: &'static str,
) -> OptionalCall {
    OptionalCall {
        builtin,
        form,
        optional_passed,
        statement,
        prints,
    }
}

/// Every builtin with an optional last argument, with and without that
/// argument, in call form and in pipe form. `channel.new` has no pipe
/// form without its optional argument: its only parameter is the
/// optional one, and a pipe always supplies an argument.
///
/// The last two rows name the builtin through an import alias and
/// through a selective import: the rule belongs to the function's
/// signature, whatever name the call reaches it by.
const OPTIONAL_CALLS: [OptionalCall; 24] = [
    optional_call("test.assert", "call", false, "test.assert(true)", ""),
    optional_call(
        "test.assert",
        "call",
        true,
        "test.assert(true, \"message a\")",
        "",
    ),
    optional_call("test.assert", "pipe", false, "true |> test.assert()", ""),
    optional_call("test.assert", "pipe", false, "yes |> test.assert", ""),
    optional_call(
        "test.assert",
        "pipe",
        true,
        "true |> test.assert(\"message b\")",
        "",
    ),
    optional_call("test.assert_eq", "call", false, "test.assert_eq(1, 1)", ""),
    optional_call(
        "test.assert_eq",
        "call",
        true,
        "test.assert_eq(2, 2, \"message c\")",
        "",
    ),
    optional_call(
        "test.assert_eq",
        "pipe",
        false,
        "3 |> test.assert_eq(3)",
        "",
    ),
    optional_call(
        "test.assert_eq",
        "pipe",
        true,
        "4 |> test.assert_eq(4, \"message d\")",
        "",
    ),
    optional_call("test.assert_ne", "call", false, "test.assert_ne(1, 2)", ""),
    optional_call(
        "test.assert_ne",
        "call",
        true,
        "test.assert_ne(3, 4, \"message e\")",
        "",
    ),
    optional_call(
        "test.assert_ne",
        "pipe",
        false,
        "5 |> test.assert_ne(6)",
        "",
    ),
    optional_call(
        "test.assert_ne",
        "pipe",
        true,
        "7 |> test.assert_ne(8, \"message f\")",
        "",
    ),
    optional_call(
        "float.to_string",
        "call",
        false,
        "println(float.to_string(1.5))",
        "1.5\n",
    ),
    optional_call(
        "float.to_string",
        "call",
        true,
        "println(float.to_string(2.5, 3))",
        "2.500\n",
    ),
    optional_call(
        "float.to_string",
        "pipe",
        false,
        "println(3.5 |> float.to_string())",
        "3.5\n",
    ),
    optional_call(
        "float.to_string",
        "pipe",
        false,
        "println(4.5 |> float.to_string)",
        "4.5\n",
    ),
    optional_call(
        "float.to_string",
        "pipe",
        true,
        "println(5.5 |> float.to_string(2))",
        "5.50\n",
    ),
    optional_call(
        "channel.new",
        "call",
        false,
        "channel.close(channel.new())",
        "",
    ),
    optional_call(
        "channel.new",
        "call",
        true,
        "println(round_trip(channel.new(4), 1))",
        "Message(1)\n",
    ),
    optional_call(
        "channel.new",
        "pipe",
        true,
        "println(round_trip(4 |> channel.new(), 2))",
        "Message(2)\n",
    ),
    optional_call(
        "channel.new",
        "pipe",
        true,
        "println(round_trip(4 |> channel.new, 3))",
        "Message(3)\n",
    ),
    optional_call(
        "test.assert",
        "call through an import alias",
        false,
        "t.assert(yes)",
        "",
    ),
    optional_call(
        "test.assert_ne",
        "call through a selective import",
        false,
        "assert_ne(9, 10)",
        "",
    ),
];

/// The table: one program makes every call of `OPTIONAL_CALLS` in turn,
/// and prints the number of each row after its call. It must pass the
/// checker and run to the end. No statement of the table is part of
/// another, so a diagnostic names its row.
///
/// Before the fix the rows in pipe form that leave the optional argument
/// out were rejected by the checker; the other rows are guards.
#[test]
fn t2_optional_last_argument_is_accepted_in_call_and_pipe_form() {
    let mut src = String::from(
        "import test\nimport test as t\nimport test.{ assert_ne }\n\
         import float\nimport channel\n\n\
         fn round_trip(c, value) {\n  channel.send(c, value)\n  channel.receive(c)\n}\n\n\
         fn main() {\n  let yes = true\n",
    );
    let mut expected = String::new();
    for (index, row) in OPTIONAL_CALLS.iter().enumerate() {
        src.push_str(&format!(
            "  {}\n  println(\"row {index}\")\n",
            row.statement
        ));
        expected.push_str(&format!("{}row {index}\n", row.prints));
    }
    src.push_str("}\n");

    let outcome = run_program("optional_table", "run", &src);
    let rejected: Vec<String> = OPTIONAL_CALLS
        .iter()
        .filter(|row| outcome.has_diagnostic(row.statement, ""))
        .map(|row| {
            format!(
                "{} in {} form, optional argument {}: `{}`",
                row.builtin,
                row.form,
                if row.optional_passed {
                    "passed"
                } else {
                    "left out"
                },
                row.statement
            )
        })
        .collect();
    assert!(
        rejected.is_empty(),
        "every row must be accepted; rejected rows: {rejected:#?}\n{outcome:?}"
    );
    assert!(
        !outcome.timed_out,
        "the table did not finish within {RUN_TIMEOUT:?}: {outcome:?}"
    );
    assert_eq!(
        outcome.code,
        Some(0),
        "the table must be accepted and run: {outcome:?}"
    );
    assert_eq!(
        outcome.stdout, expected,
        "every row must run, in order: {outcome:?}"
    );
}

/// One argument too few, on five ordinary builtins, is a check-time
/// error that states the expected and the actual count. Each of these
/// calls passed `silt check` and failed at run time.
#[test]
fn t2_missing_argument_to_ordinary_builtin_is_rejected_at_check_time() {
    let cases: [(&str, &str); 5] = [
        ("println(list.map([1, 2]))", "expects 2 arguments, got 1"),
        ("println(string.trim())", "expects 1 argument, got 0"),
        (
            "println(map.get(#{ \"a\": 1 }))",
            "expects 2 arguments, got 1",
        ),
        (
            "println(string.replace(\"aXb\", \"X\"))",
            "expects 3 arguments, got 2",
        ),
        (
            "println(list.fold([1, 2], 0))",
            "expects 3 arguments, got 2",
        ),
    ];
    let mut src = String::from("import list\nimport string\nimport map\n\nfn main() {\n");
    for (statement, _) in cases {
        src.push_str(&format!("  {statement}\n"));
    }
    src.push_str("}\n");

    let outcome = assert_check_rejects("missing_builtin", &src, &["expects"]);
    for (statement, counts) in cases {
        assert!(
            outcome.has_diagnostic(statement, counts),
            "`{statement}` must be rejected with {counts:?}: {outcome:?}"
        );
    }

    // `silt run` stops at the same errors instead of reaching a builtin.
    assert_run_refuses("missing_builtin_run", &src, "expects 2 arguments, got 1");
}

/// The same for a function of a user module.
#[test]
fn t2_missing_argument_to_user_module_function_is_rejected_at_check_time() {
    let files = [
        ("util.silt", "pub fn add(a: Int, b: Int) -> Int { a + b }\n"),
        (
            "main.silt",
            "import util\nfn main() {\n  let x: Int = util.add(1)\n  println(x)\n}\n",
        ),
    ];
    let outcome = assert_check_rejects_files(
        "missing_user_module",
        &files,
        &["expects 2 arguments, got 1"],
    );
    assert!(
        outcome.has_diagnostic("util.add(1)", "expects 2 arguments, got 1"),
        "the diagnostic must point at the call: {outcome:?}"
    );
}

/// GUARD. A user module function called with all its arguments, in call
/// form and in pipe form.
#[test]
fn t2_guard_user_module_function_with_exact_arguments_is_accepted() {
    let files = [
        ("util.silt", "pub fn add(a: Int, b: Int) -> Int { a + b }\n"),
        (
            "main.silt",
            "import util\nfn main() {\n  println(util.add(1, 2))\n  println(1 |> util.add(3))\n}\n",
        ),
    ];
    assert_runs_files("exact_user_module", &files, "3\n4\n");
}

/// GUARD. Ordinary builtins called with all their arguments, in call
/// form and in pipe form.
#[test]
fn t2_guard_ordinary_builtins_with_exact_arguments_are_accepted() {
    assert_runs(
        "exact_builtins",
        r#"
import list
import string
import map
import int
import math

fn main() {
  println(list.map([1, 2]) { x -> x + 1 })
  println(string.trim("  a  "))
  println(map.get(#{ "a": 1 }, "a"))
  println(int.to_string(7))
  println(math.pow(2.0, 3.0))
  println(list.fold([1, 2], 0) { acc, x -> acc + x })
  println(string.replace("aXb", "X", "-"))
  println([1, 2] |> list.map { x -> x + 1 })
  println("  a  " |> string.trim)
  println("  a  " |> string.trim())
  println("aXb" |> string.replace("X", "-"))
}
"#,
        "[2, 3]\na\nSome(1)\n7\n8\n3\na-b\n[2, 3]\na\na\na-b\n",
    );
}

/// GUARD. The pipe form of an ordinary builtin with one argument too
/// few was rejected before the fix and still is.
#[test]
fn t2_guard_missing_argument_in_pipe_form_is_rejected() {
    let cases: [(&str, &str); 2] = [
        (
            "println([1, 2] |> list.fold(0))",
            "expects 3 arguments, got 2",
        ),
        (
            "println(\"aXb\" |> string.replace(\"X\"))",
            "expects 3 arguments, got 2",
        ),
    ];
    let mut src = String::from("import list\nimport string\n\nfn main() {\n");
    for (statement, _) in cases {
        src.push_str(&format!("  {statement}\n"));
    }
    src.push_str("}\n");

    let outcome = assert_check_rejects("missing_pipe", &src, &["expects"]);
    for (statement, counts) in cases {
        assert!(
            outcome.has_diagnostic(statement, counts),
            "`{statement}` must be rejected with {counts:?}: {outcome:?}"
        );
    }
}

/// A builtin with an optional last argument accepts two counts and no
/// others, in call form and in pipe form. The diagnostic states both
/// accepted counts and the actual one; before the fix it named the full
/// count only.
#[test]
fn t2_wrong_argument_count_for_optional_builtin_states_both_counts() {
    let cases: [(&str, &str); 8] = [
        ("test.assert()", "expects 1 or 2 arguments, got 0"),
        (
            "test.assert(true, \"m\", \"extra\")",
            "expects 1 or 2 arguments, got 3",
        ),
        ("test.assert_eq(1)", "expects 2 or 3 arguments, got 1"),
        (
            "test.assert_ne(1, 2, \"m\", \"extra\")",
            "expects 2 or 3 arguments, got 4",
        ),
        (
            "println(float.to_string())",
            "expects 1 or 2 arguments, got 0",
        ),
        (
            "channel.close(channel.new(1, 2))",
            "expects 0 or 1 arguments, got 2",
        ),
        ("5 |> test.assert_eq()", "expects 2 or 3 arguments, got 1"),
        (
            "println(1.5 |> float.to_string(2, 3))",
            "expects 1 or 2 arguments, got 3",
        ),
    ];
    let mut src = String::from("import test\nimport float\nimport channel\n\nfn main() {\n");
    for (statement, _) in cases {
        src.push_str(&format!("  {statement}\n"));
    }
    src.push_str("}\n");

    let outcome = assert_check_rejects("optional_wrong_count", &src, &["expects"]);
    for (statement, counts) in cases {
        assert!(
            outcome.has_diagnostic(statement, counts),
            "`{statement}` must be rejected with {counts:?}: {outcome:?}"
        );
    }
}

/// GUARD. An optional argument that is passed is still type checked.
#[test]
fn t2_guard_optional_argument_is_type_checked() {
    let cases: [(&str, &str); 2] = [
        (
            "test.assert(true, 5)",
            "type mismatch: expected String, got Int",
        ),
        (
            "println(float.to_string(1.5, \"two\"))",
            "type mismatch: expected Int, got String",
        ),
    ];
    let mut src = String::from("import test\nimport float\n\nfn main() {\n");
    for (statement, _) in cases {
        src.push_str(&format!("  {statement}\n"));
    }
    src.push_str("}\n");

    let outcome = assert_check_rejects("optional_wrong_type", &src, &["type mismatch"]);
    for (statement, message) in cases {
        assert!(
            outcome.has_diagnostic(statement, message),
            "`{statement}` must be rejected with {message:?}: {outcome:?}"
        );
    }
}

// ── T3: a match whose arms all diverge ───────────────────────────────

/// The shape from the withdrawn `when let ... else match` proposal.
#[test]
fn t3_match_with_all_arms_diverging_is_a_diverging_else_body() {
    assert_runs(
        "match_else",
        r#"
fn load_data() -> Result(Int, String) { Ok(42) }
fn main() {
  let res = load_data()
  when let Ok(data) = res else {
    match res {
      Err(e) -> panic("load failed: {e}")
      Ok(_)  -> panic("unreachable")
    }
  }
  println(data)
}
"#,
        "42\n",
    );
}

/// When the `when let` pattern fails, the arm of the `else` match that
/// fits the value runs.
#[test]
fn t3_diverging_match_in_else_body_runs_the_matching_arm() {
    let outcome = run_program(
        "match_else_failure",
        "run",
        r#"
fn load_data() -> Result(Int, String) { Err("boom") }
fn main() {
  let res = load_data()
  when let Ok(data) = res else {
    match res {
      Err(e) -> panic("load failed: {e}")
      Ok(_)  -> panic("unreachable")
    }
  }
  println(data)
}
"#,
    );
    assert!(outcome.failed(), "the program panics: {outcome:?}");
    assert!(
        outcome.output().contains("load failed: boom"),
        "the `Err` arm of the else match must run: {outcome:?}"
    );
    assert!(
        !outcome.output().contains("must diverge"),
        "the program must type check: {outcome:?}"
    );
}

/// The same rule for the other forms: a guardless match, arms that
/// `return`, a match at the end of a block, and a match nested in an
/// arm of another.
#[test]
fn t3_every_match_form_with_all_arms_diverging_diverges() {
    assert_runs(
        "match_forms",
        r#"
fn load_data() -> Result(Int, String) { Ok(42) }

fn guardless(n: Int) -> Int {
  when n > 0 else {
    match {
      n == 0 -> panic("zero")
      _ -> panic("negative")
    }
  }
  n
}

fn match_with_return(res: Result(Int, String)) -> Int {
  when let Ok(data) = res else {
    match res {
      Err(e) -> return 0
      Ok(_) -> return 1
    }
  }
  data
}

fn match_in_block(res: Result(Int, String)) -> Int {
  when let Ok(data) = res else {
    println("failing")
    match res {
      Err(e) -> panic("load failed: {e}")
      Ok(_) -> panic("unreachable")
    }
  }
  data
}

fn nested_match(res: Result(Int, String), flag: Bool) -> Int {
  when let Ok(data) = res else {
    match flag {
      true -> match res {
        Err(e) -> panic("a: {e}")
        Ok(_) -> panic("unreachable")
      }
      false -> panic("b")
    }
  }
  data
}

fn main() {
  println(guardless(3))
  println(match_with_return(load_data()))
  println(match_with_return(Err("no")))
  println(match_in_block(load_data()))
  println(nested_match(load_data(), true))
}
"#,
        "3\n42\n0\n42\n42\n",
    );
}

/// GUARD. A match with an arm that produces a value does not diverge.
#[test]
fn t3_guard_match_with_an_arm_that_yields_a_value_does_not_diverge() {
    assert_check_rejects(
        "match_not_diverging",
        r#"
fn load_data() -> Result(Int, String) { Ok(42) }

fn main() {
  let res = load_data()
  when let Ok(data) = res else {
    match res {
      Err(e) -> panic("load failed: {e}")
      Ok(_) -> 7
    }
  }
  println(data)
}
"#,
        &["'when let' else body must diverge"],
    );
}

/// GUARD. A block that ends in a diverging expression diverges, also
/// after other statements and when nested.
#[test]
fn t3_guard_block_that_ends_in_a_diverging_expression_diverges() {
    assert_runs(
        "block_forms",
        r#"
fn load_data() -> Result(Int, String) { Ok(42) }

fn stmt_then_panic() -> Int {
  let res = load_data()
  when let Ok(data) = res else {
    println("failing")
    panic("load failed")
  }
  data
}

fn nested_block() -> Int {
  let res = load_data()
  when let Ok(data) = res else {
    {
      println("failing")
      panic("load failed")
    }
  }
  data
}

fn return_in_block() -> Int {
  let res = load_data()
  when let Ok(data) = res else {
    println("failing")
    return 0
  }
  data
}

fn when_bool(n: Int) -> Int {
  when n > 0 else {
    println("failing")
    return 0
  }
  n
}

fn main() {
  println(stmt_then_panic())
  println(nested_block())
  println(return_in_block())
  println(when_bool(3))
}
"#,
        "42\n42\n42\n3\n",
    );
}

/// GUARD. A match whose arms all diverge is still accepted where a
/// value is expected, and one with a value arm still has that arm's
/// type.
#[test]
fn t3_guard_match_keeps_its_type_where_a_value_is_expected() {
    assert_runs(
        "match_as_value",
        r#"
fn always_fails(n: Int) -> Int {
  match n {
    0 -> panic("zero")
    _ -> panic("other")
  }
}

fn one_value_arm(n: Int) -> Int {
  match n {
    0 -> panic("zero")
    _ -> n + 1
  }
}

fn main() {
  println(one_value_arm(1))
}
"#,
        "2\n",
    );
}

// ── T4: type errors in impl bodies ───────────────────────────────────

/// An impl written against an alias of the target type. `silt check`
/// exited 0 and `silt run` printed `not an int`.
#[test]
fn t4_type_error_in_impl_for_alias_is_reported() {
    let src = r#"
trait Foo { fn foo(self) -> Int }
type Ints = List(Int)
trait Foo for Ints {
  fn foo(self) -> Int { "not an int" }
}
fn main() { println([1, 2].foo()) }
"#;
    let outcome = assert_check_rejects(
        "impl_alias",
        src,
        &["type mismatch: expected Int, got String"],
    );
    assert!(
        outcome.has_diagnostic(
            "fn foo(self) -> Int { \"not an int\" }",
            "type mismatch: expected Int, got String"
        ),
        "the diagnostic must point at the method body: {outcome:?}"
    );
    assert_run_refuses(
        "impl_alias_run",
        src,
        "type mismatch: expected Int, got String",
    );
}

/// An impl written against `Range`, which is registered as `List`.
#[test]
fn t4_type_error_in_impl_for_range_is_reported() {
    assert_check_rejects(
        "impl_range",
        r#"
trait Foo { fn foo(self) -> Int }
trait Foo for Range(a) {
  fn foo(self) -> Int { "not an int" }
}
fn main() { println((1..3).foo()) }
"#,
        &["type mismatch: expected Int, got String"],
    );
}

/// An impl written against `Fun`, which is registered as `Fn`.
#[test]
fn t4_undefined_name_in_impl_for_fun_is_reported() {
    assert_check_rejects(
        "impl_fun",
        r#"
trait Foo { fn foo(self) -> Int }
trait Foo for Fun {
  fn foo(self) -> Int { undefined_name + 1 }
}
fn main() { println("x") }
"#,
        &["undefined variable 'undefined_name'"],
    );
}

/// The impl error is reported next to an error in an ordinary function.
/// Only the function's error was reported.
#[test]
fn t4_impl_error_is_reported_together_with_other_errors() {
    let outcome = assert_check_rejects(
        "impl_alias_and_fn",
        r#"
trait Foo { fn foo(self) -> Int }
type Ints = List(Int)
trait Foo for Ints {
  fn foo(self) -> Int { "not an int" }
}
fn helper(x) { x + 1 }
fn main() {
  let n: String = helper(1)
  println([1, 2].foo())
}
"#,
        &[],
    );
    assert!(
        outcome.has_diagnostic(
            "fn foo(self) -> Int { \"not an int\" }",
            "type mismatch: expected Int, got String"
        ),
        "the impl body error must be reported: {outcome:?}"
    );
    assert!(
        outcome.has_diagnostic(
            "let n: String = helper(1)",
            "type mismatch: expected String, got Int"
        ),
        "the function body error must be reported: {outcome:?}"
    );
}

/// GUARD. The two controls from the audit: the same impl written against
/// the canonical type, and the alias impl in a program where no scheme
/// is narrowed. Both were reported before the fix.
#[test]
fn t4_guard_impl_errors_that_were_reported_still_are() {
    assert_check_rejects(
        "impl_list",
        r#"
trait Foo { fn foo(self) -> Int }
trait Foo for List(a) {
  fn foo(self) -> Int { "not an int" }
}
fn main() { println([1, 2].foo()) }
"#,
        &["type mismatch: expected Int, got String"],
    );
    assert_check_rejects(
        "impl_alias_no_narrowing",
        r#"
trait Foo { fn foo(self) -> Int }
type Ints = List(Int)
trait Foo for Ints {
  fn foo(self) -> Int { "not an int" }
}
fn main() -> () { println([1, 2].foo()) }
"#,
        &["type mismatch: expected Int, got String"],
    );
}

/// GUARD. Correct impls against an alias and against `Range` check and
/// dispatch.
#[test]
fn t4_guard_correct_impls_for_alias_and_range_run() {
    assert_runs(
        "impl_correct",
        r#"
trait Foo { fn foo(self) -> Int }
trait Bar { fn bar(self) -> Int }
type Ints = List(Int)
trait Foo for Ints {
  fn foo(self) -> Int { 7 }
}
trait Bar for Range(a) {
  fn bar(self) -> Int { 8 }
}
fn main() {
  println([1, 2].foo())
  println((1..3).bar())
  println([4, 5].bar())
}
"#,
        "7\n8\n8\n",
    );
}
