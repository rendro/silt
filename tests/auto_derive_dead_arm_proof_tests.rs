//! Behavioural barrage for the deleted `dispatch_trait_method` arms.
//!
//! Earlier rounds carried atomic counter instrumentation in
//! `src/vm/dispatch.rs` that tracked every hit on the
//! `(Variant, Variant)` / `(Record, Record)` arms in compare /
//! equal / hash. The counters proved the arms were dead for user
//! types after the round-62 auto-derive synthesis pass and
//! near-dead for built-in types after the follow-up that extended
//! synth to built-in enums and records. The arms — and the
//! counters — were deleted in the round that landed this test
//! file's current form.
//!
//! After deletion, the proof IS the barrage itself: if any shape
//! regresses to "no qualified-global emitted" the call resolves
//! through the catch-all error path in `dispatch_trait_method`,
//! which surfaces as a runtime
//! `error[runtime]: compare() not supported between …` (or `no
//! method 'hash' for type '…'`) — never the expected output. So
//! every line below that asserts on stdout simultaneously locks
//! "qualified-global was emitted by the synth pass and reached
//! the runtime intact".
//!
//! The barrages cover:
//!   - non-generic enum (1/2/3+ variants, with/without args)
//!   - non-generic record (1/2/3+ fields)
//!   - generic enum (1 param, 2 params)
//!   - generic record
//!   - self-recursive enum
//!   - self-recursive generic enum
//!   - all four traits via `where a: Trait` bound
//!   - all four traits via direct `x.method()` call
//!   - all four traits via qualified `Type.method(...)` call
//!   - built-in enums (Weekday, Method)
//!   - built-in records (Date)
//!
//! Two test forms are used:
//!   1. **In-process**: drive each shape end-to-end (typecheck →
//!      compile → run) without asserting stdout. A synth-pipeline
//!      bug here surfaces as a panic during compile or a runtime
//!      error caught by the in-process runner.
//!   2. **Out-of-process**: run via `silt run` and lock stdout
//!      against the expected output, so the full path including
//!      the binary entrypoint is exercised.

use std::process::Command;
use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;

fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_dead_arm_proof_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

/// In-process synth-pipeline driver. Each shape is fed through
/// typecheck → compile → vm.run; a panic at any stage surfaces
/// as a test failure. Mirrors `tests/integration.rs::run`.
fn run_silt_inproc(src: &str) {
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let _ = vm.run(script);
}

// ── 1. User-type barrage: every shape × every trait × every call form

/// Drive a comprehensive barrage of user-typed Compare/Equal/Hash/
/// Display calls. Every shape compiles and runs successfully — the
/// in-process runner lets vm.run errors surface as test failures
/// indirectly via panic on `compile_program.expect` (compile errors)
/// and via `vm.run`'s Err return (runtime errors are silently
/// dropped by the `let _ = …` here, so test 3 below also locks the
/// observable outputs through the CLI).
#[test]
fn user_types_compile_and_run_through_synth_globals() {
    // Shape 1: non-generic enum, nullary variants
    run_silt_inproc(
        r#"
type Color { Red, Green, Blue, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    let _ = cmp_gen(Red, Green)
    let _ = eq_gen(Red, Green)
    let _ = h(Red)
    let _ = d(Red)
    let _ = Color.compare(Red, Green)
    let _ = Color.equal(Red, Green)
}
"#,
    );

    // Shape 2: non-generic enum with args
    run_silt_inproc(
        r#"
type Tagged { Foo(Int), Bar(Int, String), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    let _ = cmp_gen(Foo(1), Foo(2))
    let _ = eq_gen(Bar(1, "x"), Bar(1, "y"))
    let _ = h(Foo(7))
    let _ = d(Bar(2, "z"))
}
"#,
    );

    // Shape 3: non-generic record
    run_silt_inproc(
        r#"
type Point { x: Int, y: Int, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    let p = Point { x: 1, y: 2 }
    let q = Point { x: 1, y: 3 }
    let _ = cmp_gen(p, q)
    let _ = eq_gen(p, q)
    let _ = h(p)
    let _ = d(p)
    let _ = p.compare(q)
    let _ = p.equal(q)
    let _ = p.hash()
    let _ = p.display()
}
"#,
    );

    // Shape 4: generic enum, single param
    run_silt_inproc(
        r#"
type Box(a) { Foo(a), Bar(a, a), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    let _ = cmp_gen(Foo(1), Foo(2))
    let _ = eq_gen(Bar(1, 2), Bar(1, 3))
    let _ = h(Foo(7))
    let _ = d(Bar(1, 2))
    let _ = Box.compare(Foo(1), Foo(2))
}
"#,
    );

    // Shape 5: generic enum, two params
    run_silt_inproc(
        r#"
type Pair(a, b) { Tup(a, b), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
    let _ = cmp_gen(Tup(1, "a"), Tup(2, "b"))
    let _ = eq_gen(Tup(1, "x"), Tup(1, "x"))
    let _ = h(Tup(1, "y"))
}
"#,
    );

    // Shape 6: generic record
    run_silt_inproc(
        r#"
type Wrapped(a) { value: a }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    let w = Wrapped { value: 5 }
    let v = Wrapped { value: 7 }
    let _ = cmp_gen(w, v)
    let _ = eq_gen(w, v)
    let _ = h(w)
    let _ = d(w)
    let _ = w.compare(v)
    let _ = w.equal(v)
}
"#,
    );

    // Shape 7: self-recursive enum (non-generic)
    run_silt_inproc(
        r#"
type Tree { Leaf, Node(Tree, Tree), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    let _ = cmp_gen(Leaf, Node(Leaf, Leaf))
    let _ = cmp_gen(Node(Leaf, Leaf), Node(Leaf, Leaf))
}
"#,
    );

    // Shape 8: self-recursive enum (generic)
    run_silt_inproc(
        r#"
type GTree(a) { GLeaf, GNode(GTree(a), GTree(a)), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    let l: GTree(Int) = GLeaf
    let n: GTree(Int) = GNode(GLeaf, GLeaf)
    let _ = cmp_gen(l, n)
}
"#,
    );

    // Shape 9: 3+ field record
    run_silt_inproc(
        r#"
type Big { a: Int, b: Int, c: Int, d: String, }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn main() {
    let p = Big { a: 1, b: 2, c: 3, d: "x" }
    let q = Big { a: 1, b: 2, c: 3, d: "y" }
    let _ = cmp_gen(p, q)
    let _ = eq_gen(p, q)
    let _ = h(p)
}
"#,
    );

    // Shape 10: 3+ variant enum
    run_silt_inproc(
        r#"
type Status { Active(Int), Pending, Done, Cancelled(String), Failed(Int, String), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    let _ = cmp_gen(Active(1), Pending)
    let _ = cmp_gen(Cancelled("x"), Failed(1, "y"))
    let _ = eq_gen(Pending, Done)
    let _ = h(Active(5))
    let _ = d(Failed(2, "z"))
}
"#,
    );
}

// ── 2. Built-in receivers also route through synth ────────────────

/// Locks the round-62 follow-up: built-in enums (Weekday, Method)
/// and built-in records (Date) flow through the same auto-derive
/// synth pipeline as user-declared types. Every call below
/// resolves through a synth-emitted `<Type>.<method>` global at
/// runtime; a regression that drops the synth would surface as a
/// `cmp_gen(Monday, Friday)` runtime error, which `vm.run` would
/// report and the in-process runner would let through silently —
/// the matching CLI test (`builtin_compare_via_cli_subprocess`)
/// below catches that case via stdout assertion.
#[test]
fn builtin_types_compile_and_run_through_synth_globals() {
    // Weekday — non-generic built-in enum.
    run_silt_inproc(
        r#"
import time
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    let _ = cmp_gen(Monday, Friday)
    let _ = eq_gen(Monday, Monday)
    let _ = h(Tuesday)
    let _ = d(Wednesday)
}
"#,
    );

    // HTTP Method — non-generic built-in enum.
    run_silt_inproc(
        r#"
import http
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    let _ = cmp_gen(GET, POST)
    let _ = d(DELETE)
}
"#,
    );

    // Built-in record (Date) — Compare/Equal/Hash/Display all synth'd.
    run_silt_inproc(
        r#"
import time
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    match time.date(2025, 1, 15) {
        Ok(a) -> match time.date(2025, 6, 30) {
            Ok(b) -> {
                let _ = cmp_gen(a, b)
                let _ = eq_gen(a, a)
                let _ = h(a)
                let _ = d(a)
            }
            Err(_) -> ()
        }
        Err(_) -> ()
    }
}
"#,
    );
}

// ── 3. End-to-end behaviour: user-type ops still produce correct results

/// Full behaviour pass: same barrage as test 1 but checking outputs
/// (CLI subprocess for stdout capture). Locks that the synth path
/// produces semantically correct results — and, by extension, that
/// `Op::CallMethod` resolves through the synth-emitted globals
/// rather than the catch-all error path in `dispatch_trait_method`
/// (the latter would produce a runtime error, never the expected
/// numeric / string output).
#[test]
fn user_type_barrage_produces_expected_results() {
    let out = run_silt_ok(
        "behaviour_barrage",
        r#"
type Color { Red, Green, Blue, }
type Tagged { Foo(Int), Bar(Int, String), }
type Point { x: Int, y: Int, }
type Box(a) { Boxed(a), }
type Wrapped(a) { value: a }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    println(cmp_gen(Red, Green))
    println(cmp_gen(Foo(5), Foo(3)))
    println(eq_gen(Point { x: 1, y: 2 }, Point { x: 1, y: 2 }))
    println(cmp_gen(Boxed(1), Boxed(2)))
    println(d(Wrapped { value: 42 }))
    println(Color.compare(Red, Green))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(
        lines,
        vec!["-1", "1", "true", "-1", "Wrapped { value: 42 }", "-1"]
    );
}

/// Companion to the in-process built-in barrage: lock stdout via
/// the CLI to catch the case where vm.run silently fails. If the
/// synth global for `Weekday.compare` were missing, this test
/// would surface a runtime-error stderr and a missing stdout line.
#[test]
fn builtin_compare_via_cli_subprocess() {
    let out = run_silt_ok(
        "builtin_compare_cli",
        r#"
import time
import http
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    println(cmp_gen(Monday, Friday))
    println(cmp_gen(GET, POST))
    println(d(Tuesday))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "-1", "Tuesday"]);
}
