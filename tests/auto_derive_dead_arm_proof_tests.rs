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
//!      compile → run). `run_silt_inproc` asserts `vm.run` returns
//!      Ok, so a runtime error from any synth path (catch-all in
//!      `dispatch_trait_method`, missing qualified-global, dispatch
//!      arm regression, etc.) surfaces as a test failure. Stdout is
//!      not asserted here.
//!   2. **Out-of-process**: run via `silt run` and lock stdout
//!      against the expected output, so the full path including
//!      the binary entrypoint is exercised. Shapes 1-6 are covered
//!      by `user_type_barrage_produces_expected_results`; shapes
//!      7-10 (self-recursive enums and 3+-field/variant types) by
//!      `user_type_barrage_extended_shapes_produce_expected_results`.

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
/// typecheck → compile → vm.run; a panic or runtime error at any
/// stage surfaces as a test failure. Mirrors `tests/integration.rs::run`.
///
/// Earlier rounds discarded the `vm.run` Err return with `let _ = …`,
/// trusting Test 3's CLI-subprocess pass to lock observable outputs.
/// That left a behavioural gap for any shape (7-10: self-recursive
/// enums and 3+-field/variant types) not exercised by Test 3:
/// a dispatch regression like an off-by-one in `dispatch_trait_method`
/// affecting only those shapes would surface as a silent runtime
/// error that no assertion caught. We now require `vm.run` to succeed
/// and panic on Err so every shape fed through this driver locks
/// "no runtime error from the synth path" end-to-end.
fn run_silt_inproc(src: &str) {
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    if let Err(err) = vm.run(script) {
        panic!(
            "vm.run failed in run_silt_inproc — synth-pipeline regression in dispatch_trait_method or an arm above it: {err:?}"
        );
    }
}

// ── 1. User-type barrage: every shape × every trait × every call form

/// Drive a comprehensive barrage of user-typed Compare/Equal/Hash/
/// Display calls. Every shape compiles and runs successfully —
/// `run_silt_inproc` panics on either a compile error
/// (`compile_program.expect`) or a runtime error (`vm.run` Err is
/// asserted). The companion CLI tests
/// (`user_type_barrage_produces_expected_results` for shapes 1-6 and
/// `user_type_barrage_extended_shapes_produce_expected_results` for
/// shapes 7-10) additionally lock the observable outputs end-to-end
/// through the `silt run` binary.
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
/// `cmp_gen(Monday, Friday)` runtime error and fail the test via
/// `run_silt_inproc`'s `vm.run` assertion. The matching CLI test
/// (`builtin_compare_via_cli_subprocess`) below additionally locks
/// the observable outputs via stdout assertion.
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

/// Companion to Test 3 for shapes that the original behaviour pass
/// did not exercise: self-recursive enum (`Tree`), self-recursive
/// generic enum (`GTree(a)`), 3+-field record (`Big`), and 3+-variant
/// enum (`Status`). Without this, a regression in
/// `dispatch_trait_method` (or in the auto-derive synth pass) that
/// only affected these shapes would not fail any test — the
/// in-process barrage above would either panic (now that
/// `run_silt_inproc` asserts vm.run success) OR, for stricter
/// behavioural correctness (e.g. wrong tag ordinal in `cmp`, wrong
/// field count in `display`), would still execute without error but
/// produce a wrong value that nothing checks.
///
/// Locking stdout via CLI for these four shapes closes both holes
/// simultaneously: silent runtime errors surface as a missing stdout
/// line, and silent semantic regressions surface as a mismatch on
/// the asserted output strings.
#[test]
fn user_type_barrage_extended_shapes_produce_expected_results() {
    let out = run_silt_ok(
        "extended_shapes_barrage",
        r#"
type Tree { Leaf, Node(Tree, Tree), }
type GTree(a) { GLeaf, GNode(GTree(a), GTree(a)), }
type Big { a: Int, b: Int, c: Int, d: String, }
type Status { Active(Int), Pending, Done, Cancelled(String), Failed(Int, String), }
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    -- Shape 7: self-recursive enum
    println(cmp_gen(Leaf, Node(Leaf, Leaf)))
    println(eq_gen(Node(Leaf, Leaf), Node(Leaf, Leaf)))
    println(d(Node(Leaf, Leaf)))

    -- Shape 8: self-recursive generic enum
    let gl: GTree(Int) = GLeaf
    let gn: GTree(Int) = GNode(GLeaf, GLeaf)
    println(cmp_gen(gl, gn))
    println(d(gl))

    -- Shape 9: 3+ field record
    let p = Big { a: 1, b: 2, c: 3, d: "x" }
    let q = Big { a: 1, b: 2, c: 3, d: "y" }
    let r = Big { a: 1, b: 2, c: 3, d: "x" }
    println(cmp_gen(p, q))
    println(eq_gen(p, r))
    println(d(p))

    -- Shape 10: 3+ variant enum
    println(cmp_gen(Active(1), Pending))
    println(cmp_gen(Cancelled("x"), Failed(1, "y")))
    println(eq_gen(Pending, Done))
    println(d(Failed(2, "z")))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(
        lines,
        vec![
            // Shape 7
            "-1",
            "true",
            "Node(Leaf, Leaf)",
            // Shape 8
            "-1",
            "GLeaf",
            // Shape 9
            "-1",
            "true",
            "Big { a: 1, b: 2, c: 3, d: x }",
            // Shape 10
            "-1",
            "-1",
            "false",
            "Failed(2, z)",
        ]
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
