//! Behaviour locks for the compiler lane of fix wave 1.
//!
//! C1  A `match` with a scrutinee, a block with `let`, or a `loop` used as
//!     an operand that is not the first one (right side of `+`, an
//!     argument, a later element of a literal, ...) computed wrong values
//!     or failed at run time: the local it introduced got the stack slot
//!     of an operand evaluated before it.
//! C2  The unit pattern `()` never matched.
//! C3  A `loop` without bindings destroyed the locals of the enclosing
//!     function when it was re-entered.
//! C4  `json.parse` / `toml.parse` returned values that did not have the
//!     declared type of the record they were decoded into, and did not
//!     resolve type aliases.
//! C5  A top-level `let` could not use a declaration written after it.
//! C6  A pattern that can fail, in a closure parameter, bound its names
//!     to the parts of a value of another shape.
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
    let name = format!("silt_wave1_compiler_{pid}_{unique}_{label}");
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

/// `silt check` and `silt run` must both reject the program with an
/// `error[compile]` diagnostic that contains every one of `needles`, and
/// `silt run` must not execute any of it.
fn assert_compile_error(label: &str, src: &str, needles: &[&str]) {
    for subcommand in ["check", "run"] {
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
}

// ── C1: constructs that introduce locals, as operands ────────────────

/// Declarations shared by the programs of the C1 matrix.
const PRELUDE: &str = r#"import int
import list

fn add(a, b) {
  a + b
}

"#;

/// The three constructs that introduce locals: their name, the
/// expression, and its value. The expressions are written to stand
/// inside `fn main`, where `x` is 5.
const CONSTRUCTS: [(&str, &str); 3] = [
    (
        "match",
        "match x {
    5 -> 10
    _ -> 20
  }",
    ),
    (
        "block",
        "{
    let y = 10
    y * 2
  }",
    ),
    (
        "loop",
        "loop i = 0, acc = 100 {
    match {
      i >= 3 -> acc + 1000
      _ -> loop(i + 1, acc + i)
    }
  }",
    ),
];

/// Run one program per construct: `statement` with `<E>` replaced by the
/// construct, as the body of `main`. `expected` gives the output for the
/// match (value 10), the block (value 20) and the loop (value 1103).
fn assert_in_position(position: &str, statement: &str, expected: [&str; 3]) {
    for ((name, expr), expected) in CONSTRUCTS.iter().zip(expected) {
        let body = statement.replace("<E>", expr);
        let src = format!("{PRELUDE}fn main() {{\n  let x = 5\n{body}}}\n");
        assert_prints(
            &format!("c1_{position}_{name}"),
            &src,
            &format!("{expected}\n"),
        );
    }
}

#[test]
fn c1_right_operand_of_plus() {
    assert_in_position(
        "plus",
        "  let r = 1 + <E>\n  println(r)\n",
        ["11", "21", "1104"],
    );
}

#[test]
fn c1_second_element_of_a_tuple() {
    assert_in_position(
        "tuple",
        "  let r = (1, <E>)\n  println(r)\n",
        ["(1, 10)", "(1, 20)", "(1, 1103)"],
    );
}

#[test]
fn c1_element_of_a_list() {
    assert_in_position(
        "list",
        "  let r = [1, 2, <E>]\n  println(r)\n",
        ["[1, 2, 10]", "[1, 2, 20]", "[1, 2, 1103]"],
    );
}

#[test]
fn c1_argument_of_a_user_function() {
    assert_in_position(
        "user_fn",
        "  let r = add(100, <E>)\n  println(r)\n",
        ["110", "120", "1203"],
    );
}

#[test]
fn c1_argument_of_a_builtin() {
    assert_in_position(
        "builtin",
        "  let r = int.min(100, <E>)\n  println(r)\n",
        ["10", "20", "100"],
    );
}

#[test]
fn c1_argument_of_a_constructor() {
    assert_in_position(
        "constructor",
        "  let r: Result(Int, String) = Ok(<E>)\n  println(r)\n",
        ["Ok(10)", "Ok(20)", "Ok(1103)"],
    );
}

#[test]
fn c1_argument_of_println() {
    assert_in_position("println", "  println(<E>)\n", ["10", "20", "1103"]);
}

#[test]
fn c1_inside_string_interpolation() {
    assert_in_position(
        "interpolation",
        "  let r = \"x={x} v={<E>}\"\n  println(r)\n",
        ["x=5 v=10", "x=5 v=20", "x=5 v=1103"],
    );
}

#[test]
fn c1_right_side_of_a_pipe() {
    assert_in_position(
        "pipe",
        "  let r = 100 |> add(<E>)\n  println(r)\n",
        ["110", "120", "1203"],
    );
}

#[test]
fn c1_nested_two_deep() {
    assert_in_position(
        "nested",
        "  let r = 1 + (2 * <E>)\n  println(r)\n",
        ["21", "41", "2207"],
    );
    assert_in_position(
        "nested_mixed",
        "  let r = (0, add(1, <E>))\n  println(r)\n",
        ["(0, 11)", "(0, 21)", "(0, 1104)"],
    );
}

/// The remaining constructs that keep operands on the stack: record
/// literal, record update, map and set literal, range, comparison,
/// method call, list with a spread, and an operator with a construct on
/// both sides.
#[test]
fn c1_other_operand_positions() {
    let src = r#"
import list
import map

type P { a: Int, b: Int }

trait Scale {
  fn scale(self, by: Int) -> Int
}

trait Scale for Int {
  fn scale(self, by: Int) -> Int {
    self * by
  }
}

fn main() {
  let x = 5
  println(P { a: 1, b: match x {
    5 -> 50
    _ -> 0
  } })
  let p = P { a: 1, b: 2 }
  println(p.{ a: 7, b: {
    let y = 4
    y * 2
  } })
  let m = #{ "k": match x {
    5 -> 9
    _ -> 0
  } }
  println(map.get(m, "k"))
  println(#[1, match x {
    5 -> 2
    _ -> 0
  }])
  println(list.length(1..match x {
    5 -> 4
    _ -> 0
  }))
  println(1000 > match x {
    5 -> 50
    _ -> 0
  })
  println(3.scale(match x {
    5 -> 10
    _ -> 0
  }))
  let rest = [8, 9]
  println([1, ..rest, match x {
    5 -> 10
    _ -> 0
  }])
  println(match x {
    5 -> 50
    _ -> 0
  } + match x {
    5 -> 1
    _ -> 0
  })
}
"#;
    assert_prints(
        "c1_other_positions",
        src,
        "P {a: 1, b: 50}\n\
         P {a: 7, b: 8}\n\
         Some(9)\n\
         #[1, 2]\n\
         4\n\
         true\n\
         30\n\
         [1, 8, 9, 10]\n\
         51\n",
    );
}

/// A construct inside a construct inside an operand.
#[test]
fn c1_construct_inside_construct() {
    let src = r#"
fn main() {
  let x = 5
  println(1 + {
    let y = 2
    y + match x {
      5 -> {
        let z = 10
        z + loop i = 0 {
          match {
            i >= 3 -> i
            _ -> loop(i + 1)
          }
        }
      }
      _ -> 20
    }
  })
}
"#;
    assert_prints("c1_construct_in_construct", src, "16\n");
}

/// The body of a closure is compiled with the same rules.
#[test]
fn c1_inside_a_closure_body() {
    let src = r#"
import list

fn main() {
  let r = [1, 2] |> list.map { n ->
    10 + match n {
      1 -> 1
      _ -> 2
    }
  }
  println(r)
}
"#;
    assert_prints("c1_closure_body", src, "[11, 12]\n");
}

/// A closure that captures a local introduced while an operand is
/// pending must capture that local, not the operand.
#[test]
fn c1_closure_captures_a_local_introduced_in_an_operand() {
    let src = r#"
fn main() {
  println(100 + {
    let y = 10
    let f = { n -> n + y }
    f(1)
  })
}
"#;
    assert_prints("c1_capture", src, "111\n");
}

/// Arms that fail at different depths of a nested pattern, and an arm
/// whose guard fails after its names were bound, must all leave the
/// pending operand alone.
#[test]
fn c1_failed_arms_leave_the_operand_alone() {
    let src = r#"
fn classify(p) {
  1000 + match p {
    (Some(1), _) -> 1
    (Some(n), true) when n > 5 -> 2
    (Some(n), true) -> 3 + n
    (None, _) -> 4
    _ -> 5
  }
}

fn main() {
  println(classify((Some(1), false)))
  println(classify((Some(9), true)))
  println(classify((Some(2), true)))
  println(classify((None, true)))
  println(classify((Some(2), false)))
}
"#;
    assert_prints("c1_failed_arms", src, "1001\n1002\n1005\n1004\n1005\n");
}

/// An or-pattern that binds a name at different positions of its
/// alternatives, in an operand.
#[test]
fn c1_or_pattern_with_bindings_in_an_operand() {
    let src = r#"
type S { A(Int), B(String, Int), C }

fn value(s) {
  100 + match s {
    A(n) | B(_, n) -> n
    C -> 0
  }
}

fn main() {
  println(value(A(1)))
  println(value(B("b", 2)))
  println(value(C))
}
"#;
    assert_prints("c1_or_pattern", src, "101\n102\n100\n");
}

/// `when let` in a block that is an operand, on both of its paths.
#[test]
fn c1_when_let_in_an_operand_block() {
    let src = r#"
fn f(opt) {
  10 + {
    when let Some(v) = opt else {
      return -1
    }
    v
  }
}

fn main() {
  println(f(Some(5)))
  println(f(None))
}
"#;
    assert_prints("c1_when_let_operand", src, "15\n-1\n");
}

/// The else body of a `when let` whose nested pattern failed is compiled
/// for a frame without the pattern's names: a construct in it works, and
/// a name it uses is the one of the enclosing scope.
#[test]
fn c1_when_let_else_body() {
    let src = r#"
fn f(p) {
  when let (Some(a), 1) = p else {
    return 1 + match 2 {
      2 -> 10
      _ -> 20
    }
  }
  a
}

fn pick(opt, x) {
  when let Some(x) = opt else {
    return x + 100
  }
  x
}

fn main() {
  println(f((Some(5), 1)))
  println(f((Some(5), 2)))
  println(f((None, 1)))
  println(pick(Some(1), 7))
  println(pick(None, 7))
}
"#;
    assert_prints("c1_when_let_else", src, "5\n11\n11\n1\n107\n");
}

/// A top-level initialiser is compiled with the same rules.
#[test]
fn c1_top_level_let() {
    let src = r#"
let g = 1 + match 2 {
  2 -> 10
  _ -> 0
}

fn main() {
  println(g)
}
"#;
    assert_prints("c1_top_level_let", src, "11\n");
}

/// A loop in an operand position that runs many times must not grow the
/// frame with every iteration.
#[test]
fn c1_long_loop_in_an_operand() {
    let src = r#"
fn main() {
  println(1 + loop i = 0 {
    match i >= 200000 {
      true -> i
      _ -> loop(i + 1)
    }
  })
}
"#;
    assert_prints("c1_long_loop", src, "200001\n");
}

/// The same constructs as the FIRST operand, and as the value of a
/// `let`: no operand is pending when they run.
#[test]
fn guard_c1_first_operand_and_statement_positions() {
    assert_in_position(
        "guard_first_operand",
        "  let r = <E> + 1\n  println(r)\n",
        ["11", "21", "1104"],
    );
    assert_in_position(
        "guard_statement",
        "  let r = <E>\n  let s = r + x\n  println(s)\n",
        ["15", "25", "1108"],
    );
}

/// Deep tail recursion through a `match` and through a block with `let`
/// still runs in constant stack space.
#[test]
fn guard_c1_tail_calls_through_match_and_block() {
    let src = r#"
fn count(n, acc) {
  match n {
    0 -> acc
    _ -> {
      let next = acc + 1
      count(n - 1, next)
    }
  }
}

fn main() {
  println(count(300000, 0))
}
"#;
    assert_prints("guard_c1_tail_calls", src, "300000\n");
}

// ── C2: the unit pattern ─────────────────────────────────────────────

#[test]
fn c2_unit_pattern_inside_a_constructor() {
    let src = r#"
fn main() {
  let v: Result((), String) = Ok(())
  let r = match v {
    Ok(()) -> "ok-unit"
    Err(e) -> e
  }
  println(r)
}
"#;
    assert_prints("c2_ok_unit", src, "ok-unit\n");
}

#[test]
fn c2_unit_pattern_at_the_top_of_an_arm() {
    let src = r#"
fn nothing() {
  ()
}

fn main() {
  let r = match nothing() {
    () -> "unit"
  }
  println(r)
}
"#;
    assert_prints("c2_bare_unit", src, "unit\n");
}

#[test]
fn c2_unit_pattern_inside_a_tuple() {
    let src = r#"
fn main() {
  let r = match (1, ()) {
    (n, ()) -> n
  }
  println(r)
}
"#;
    assert_prints("c2_tuple_unit", src, "1\n");
}

/// The unit pattern as an alternative of an or-pattern goes through the
/// second pattern-test compiler.
#[test]
fn c2_unit_pattern_in_an_or_pattern() {
    let src = r#"
fn show(v: Result((), String)) -> String {
  match v {
    Ok(()) | Err("fine") -> "good"
    Err(e) -> e
  }
}

fn main() {
  println(show(Ok(())))
  println(show(Err("fine")))
  println(show(Err("bad")))
}
"#;
    assert_prints("c2_or_unit", src, "good\ngood\nbad\n");
}

#[test]
fn guard_c2_unit_pattern_in_let() {
    let src = r#"
fn main() {
  let (u, ()) = (1, ())
  println(u)
}
"#;
    assert_prints("guard_c2_let_unit", src, "1\n");
}

// ── C3: loop without bindings ────────────────────────────────────────

#[test]
fn c3_loop_without_bindings_keeps_the_enclosing_locals() {
    let src = r#"
import channel

fn drain(ch, label) {
  let prefix = "got"
  loop {
    match channel.try_receive(ch) {
      Message(v) -> {
        println("{prefix} {label} {v}")
        loop()
      }
      _ -> "done {label}"
    }
  }
}

fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.send(ch, 2)
  channel.send(ch, 3)
  println(drain(ch, "x"))
}
"#;
    assert_prints("c3_drain", src, "got x 1\ngot x 2\ngot x 3\ndone x\n");
}

/// The variant with a parameter that is never read: re-entry failed as
/// soon as the body bound anything.
#[test]
fn c3_loop_without_bindings_with_an_unused_parameter() {
    let src = r#"
import channel

let ch = channel.new(10)

fn drain(unused) {
  loop {
    match channel.try_receive(ch) {
      Message(v) -> {
        println("got {v}")
        loop()
      }
      _ -> "done"
    }
  }
}

fn main() {
  channel.send(ch, 1)
  channel.send(ch, 2)
  println(drain(0))
}
"#;
    assert_prints("c3_unused_param", src, "got 1\ngot 2\ndone\n");
}

/// A loop without bindings as an operand, below a pending value.
#[test]
fn c3_loop_without_bindings_in_an_operand() {
    let src = r#"
import channel

fn total(ch, base) {
  base + loop {
    match channel.try_receive(ch) {
      Message(v) when v < 3 -> loop()
      Message(v) -> v
      _ -> 0
    }
  }
}

fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.send(ch, 2)
  channel.send(ch, 3)
  println(total(ch, 100))
}
"#;
    assert_prints("c3_operand", src, "103\n");
}

// ── C4: json.parse / toml.parse ──────────────────────────────────────

/// JSON that does not fit a `Map(String, Int)` or a tuple field is an
/// error. It used to be accepted, and the record then held strings in
/// fields of those types.
#[test]
fn c4_json_of_the_wrong_shape_for_map_and_tuple_fields_is_an_error() {
    let src = r#"
import json
import map

type M { m: Map(String, Int), t: (Int, Int) }

fn main() {
  match json.parse("\{\"m\": \"oops\", \"t\": \"also\"\}", M) {
    Ok(v) -> {
      println(v)
      println(map.length(v.m))
    }
    Err(e) -> println("err: {e.message()}")
  }
  match json.parse("\{\"m\": \{\"a\": \"one\"\}, \"t\": [1, 2]\}", M) {
    Ok(v) -> println(v)
    Err(e) -> println("err: {e.message()}")
  }
  match json.parse("\{\"m\": \{\"a\": 1\}, \"t\": [1, 2, 3]\}", M) {
    Ok(v) -> println(v)
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_prints(
        "c4_json_wrong_shape",
        src,
        "err: json type mismatch: expected Map, got string\n\
         err: json type mismatch: expected Int, got string\n\
         err: expected an array of 2 elements for a tuple, got 3\n",
    );
}

/// JSON that fits is decoded into values of the declared types.
#[test]
fn c4_json_map_and_tuple_fields_are_decoded() {
    let src = r#"
import json
import map

type M { m: Map(String, Int), t: (Int, String), o: Option((Int, Int)) }

fn main() {
  match json.parse("\{\"m\": \{\"a\": 1, \"b\": 2\}, \"t\": [7, \"seven\"]\}", M) {
    Ok(v) -> {
      println(map.length(v.m))
      println(map.get(v.m, "b"))
      let (n, s) = v.t
      println(n + 1)
      println(s)
      println(v.o)
    }
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_prints("c4_json_map_tuple", src, "2\nSome(2)\n8\nseven\nNone\n");
}

#[test]
fn c4_toml_map_and_tuple_fields() {
    let src = r#"
import toml
import map

type M { m: Map(String, Int), t: (Int, String) }

fn main() {
  match toml.parse("t = [7, \"seven\"]\n[m]\na = 1\nb = 2\n", M) {
    Ok(v) -> {
      println(map.length(v.m))
      let (n, s) = v.t
      println(n + 1)
      println(s)
    }
    Err(e) -> println("err: {e.message()}")
  }
  match toml.parse("m = \"oops\"\nt = \"also\"\n", M) {
    Ok(v) -> println(v)
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_prints(
        "c4_toml_map_tuple",
        src,
        "2\n8\nseven\nerr: toml type mismatch: expected Map, got string\n",
    );
}

/// Field types written as aliases, also parametric ones and ones
/// declared after the record, are decoded as their target.
#[test]
fn c4_alias_field_types_resolve() {
    let src = r#"
import json
import toml

type Post {
  title: String,
  tags: Tags,
  n: Count,
  pair: Pair(Int),
}

type Tags = List(String)
type Count = Int
type Pair(a) = (a, a)

fn main() {
  let raw = "\{\"title\": \"hi\", \"tags\": [\"a\", \"b\"], \"n\": 3, \"pair\": [1, 2]\}"
  match json.parse(raw, Post) {
    Ok(p) -> println("ok: {p.title} {p.tags} {p.n + 1} {p.pair}")
    Err(e) -> println("err: {e.message()}")
  }
  match toml.parse("title = \"hi\"\ntags = [\"a\", \"b\"]\nn = 3\npair = [1, 2]\n", Post) {
    Ok(p) -> println("ok: {p.title} {p.tags} {p.n + 1} {p.pair}")
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_prints(
        "c4_aliases",
        src,
        "ok: hi [a, b] 4 (1, 2)\nok: hi [a, b] 4 (1, 2)\n",
    );
}

/// A record with a field no decoder exists for cannot be the target of
/// a decoding call: compile error at the call, naming field and type.
#[test]
fn c4_undecodable_field_is_a_compile_error() {
    let set_field = r#"
import json

type Bag { name: String, items: Set(Int) }

fn main() {
  match json.parse("\{\"name\": \"b\", \"items\": \"oops\"\}", Bag) {
    Ok(v) -> println(v.name)
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_compile_error(
        "c4_set_field",
        set_field,
        &["json.parse", "Bag", "items", "Set(Int)"],
    );

    let enum_field = r#"
import toml

type Color { Red, Green }
type Pixel { x: Int, c: Color }

fn main() {
  match toml.parse("x = 1\nc = \"Red\"\n", Pixel) {
    Ok(v) -> println(v.x)
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_compile_error(
        "c4_enum_field",
        enum_field,
        &["toml.parse", "Pixel", "`c`", "Color"],
    );

    let generic_record = r#"
import json

type Wrap(a) { value: a }

fn main() {
  let r: Result(Wrap(Int), JsonError) = json.parse("\{\"value\": \"x\"\}", Wrap)
  match r {
    Ok(w) -> println(w.value + 1)
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_compile_error(
        "c4_generic_record",
        generic_record,
        &["json.parse", "Wrap", "value", "`a`"],
    );

    let nested = r#"
import json

type Inner { key: Map(Int, String) }
type Outer { inners: List(Inner) }

fn main() {
  match json.parse_list("[]", Outer) {
    Ok(v) -> println(v)
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_compile_error(
        "c4_nested",
        nested,
        &[
            "json.parse_list",
            "Outer",
            "Inner",
            "key",
            "Map(Int, String)",
        ],
    );

    let piped = r#"
import json

type Bag { items: Set(Int) }

fn main() {
  let r = "\{\"items\": \"oops\"\}" |> json.parse(Bag)
  match r {
    Ok(v) -> println(v)
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_compile_error(
        "c4_piped",
        piped,
        &["json.parse", "Bag", "items", "Set(Int)"],
    );
}

/// When the type reaches the decoder through a `type a` parameter, the
/// compiler cannot see it at the call. The decoder then refuses the
/// field at run time.
#[test]
fn c4_undecodable_field_behind_a_type_parameter_is_an_error_at_run_time() {
    let src = r#"
import json
import toml

type Bag { name: String, items: Set(Int) }

fn from_json(text: String, type a) -> Result(a, JsonError) {
  json.parse(text, a)
}

fn from_toml(text: String, type a) -> Result(a, TomlError) {
  toml.parse(text, a)
}

fn main() {
  match from_json("\{\"name\": \"b\", \"items\": \"oops\"\}", Bag) {
    Ok(v) -> println(v.name)
    Err(e) -> println("err: {e.message()}")
  }
  match from_toml("name = \"b\"\nitems = \"oops\"\n", Bag) {
    Ok(v) -> println(v.name)
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_prints(
        "c4_type_parameter",
        src,
        "err: a value of type Set(Int) cannot be decoded\n\
         err: a value of type Set(Int) cannot be decoded\n",
    );
}

/// A record with a field that cannot be decoded is fine as long as it
/// is not the target of a decoding call.
#[test]
fn guard_c4_records_that_are_not_decoded_may_have_any_field_type() {
    let src = r#"
import json
import set

type Bag { name: String, items: Set(Int) }
type Label { text: String }

fn main() {
  let b = Bag { name: "b", items: #[1, 2] }
  println(set.length(b.items))
  match json.parse("\{\"text\": \"t\"\}", Label) {
    Ok(v) -> println(v.text)
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_prints("guard_c4_not_decoded", src, "2\nt\n");
}

#[test]
fn guard_c4_supported_field_types_decode_as_before() {
    let src = r#"
import json
import toml

type Address { city: String }
type User {
  name: String,
  age: Int,
  score: Float,
  active: Bool,
  tags: List(String),
  nick: Option(String),
  home: Address,
  seen: Date,
}

fn main() {
  let raw = "\{\"name\": \"A\", \"age\": 3, \"score\": 1.5, \"active\": true, \"tags\": [\"x\"], \"home\": \{\"city\": \"B\"\}, \"seen\": \"2024-03-15\"\}"
  match json.parse(raw, User) {
    Ok(u) -> println("{u.name} {u.age} {u.score} {u.active} {u.tags} {u.nick} {u.home.city} {u.seen.year}")
    Err(e) -> println("err: {e.message()}")
  }
  match json.parse("\{\"name\": 1\}", User) {
    Ok(u) -> println(u.name)
    Err(e) -> println("err: {e.message()}")
  }
  let text = "name = \"A\"\nage = 3\nscore = 1.5\nactive = true\ntags = [\"x\"]\nseen = 2024-03-15\n[home]\ncity = \"B\"\n"
  match toml.parse(text, User) {
    Ok(u) -> println("{u.name} {u.age} {u.score} {u.active} {u.tags} {u.nick} {u.home.city} {u.seen.year}")
    Err(e) -> println("err: {e.message()}")
  }
}
"#;
    assert_prints(
        "guard_c4_supported",
        src,
        "A 3 1.5 true [x] None B 2024\n\
         err: json type mismatch: expected String, got number\n\
         A 3 1.5 true [x] None B 2024\n",
    );
}

// ── C5: declarations are available to top-level initialisers ─────────

#[test]
fn c5_top_level_let_calls_a_function_declared_later() {
    let src = r#"
let x = compute()

fn compute() {
  42
}

fn main() {
  println(x)
}
"#;
    assert_prints("c5_later_fn", src, "42\n");
}

#[test]
fn c5_top_level_let_uses_a_trait_impl_declared_later() {
    let src = r#"
type Money { cents: Int }

let shown = Money { cents: 5 }.display()

trait Display for Money {
  fn display(self) -> String {
    "USD {self.cents}"
  }
}

fn main() {
  println(shown)
}
"#;
    assert_prints("c5_later_impl", src, "USD 5\n");
}

/// The same inside an imported module.
#[test]
fn c5_module_level_let_calls_a_function_declared_later() {
    let module = r#"
let cached = compute()

fn compute() {
  41 + 1
}

pub fn get() {
  cached
}
"#;
    let main = r#"
import helper

fn main() {
  println(helper.get())
}
"#;
    assert_prints_files(
        "c5_module",
        &[("main.silt", main), ("helper.silt", module)],
        "42\n",
    );
}

/// A qualified enum variant in a function that is written before the
/// enum.
#[test]
fn c5_enum_variant_used_before_the_enum_is_declared() {
    let src = r#"
fn first() {
  Color.Red
}

fn describe() {
  Color.Green.display()
}

type Color { Red, Green }

fn main() {
  println(first())
  println(describe())
}
"#;
    assert_prints("c5_enum_order", src, "Red\nGreen\n");
}

#[test]
fn guard_c5_top_level_lets_run_in_source_order() {
    let src = r#"
fn twice(n) {
  n * 2
}

let a = 1
let b = a + 1
let c = twice(b)

fn main() {
  println(a)
  println(b)
  println(c)
}
"#;
    assert_prints("guard_c5_order", src, "1\n2\n4\n");
}

// ── C6: patterns in binding positions ────────────────────────────────

/// A closure parameter pattern that does not match the argument must
/// stop the program (at check time or at run time), not bind the name
/// to a part of the other variant. Before the fix this printed `[1, 7]`.
#[test]
fn c6_refutable_closure_parameter_does_not_bind_another_variant() {
    let src = r#"
import list

type S { A(Int), B(Int) }

fn main() {
  let r = [A(1), B(7)] |> list.map { A(n) -> n }
  println(r)
}
"#;
    let out = run_silt("c6_refutable_param", "run", &[("main.silt", src)]);
    assert!(
        !out.timed_out,
        "the program hung and was killed after {RUN_TIMEOUT:?}\n{out:#?}"
    );
    assert_ne!(
        out.code,
        Some(0),
        "a closure parameter pattern that does not match must fail the program\n{out:#?}"
    );
    assert_eq!(
        out.stdout, "",
        "nothing may be computed from the mis-bound value\n{out:#?}"
    );
    assert!(
        out.stderr.contains("error"),
        "the failure must be reported as an error\n{out:#?}"
    );
}

/// Patterns that always match keep working in binding positions. This
/// includes a nominal record pattern on a value that was written as an
/// anonymous record.
#[test]
fn guard_c6_irrefutable_patterns_in_binding_positions() {
    let src = r#"
import list

type P { x: Int, y: Int }
type W { Wrap(Int) }

fn sum(p: P) -> Int {
  let P { x, y } = p
  x + y
}

fn main() {
  let (a, b) = (1, 2)
  println(a + b)
  let P { x, y } = P { x: 3, y: 4 }
  println(x + y)
  let Wrap(n) = Wrap(5)
  println(n)
  let ((c, d), e) = ((1, 2), 3)
  println(c + d + e)
  let {name, age} = {name: "A", age: 3}
  println("{name} {age}")
  println(sum({x: 3, y: 4}))
  let pairs = [(1, 2), (3, 4)]
  println(pairs |> list.map { (l, r) -> l * r })
}
"#;
    assert_prints("guard_c6_irrefutable", src, "3\n7\n5\n6\nA 3\n7\n[2, 12]\n");
}
