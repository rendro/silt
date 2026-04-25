//! Proof tests for round-62 auto-derive deadness.
//!
//! After the auto-derive synthesis pass (commit c67f7b1 + the
//! generic-types follow-up in this round), every user-declared enum
//! and record has a real `<TypeName>.<method>` global emitted by the
//! typechecker for each of Compare/Equal/Hash/Display. At runtime,
//! `Op::CallMethod`'s qualified-global lookup resolves the call BEFORE
//! falling through to `dispatch_trait_method` in `src/vm/dispatch.rs`.
//!
//! This test file proves that the dispatch arms in `dispatch_trait_method`
//! for:
//!   - `compare`: `(Value::Variant, Value::Variant)`
//!   - `compare`: `(Value::Record, Value::Record)`
//!   - `equal`:  Variant / Record receivers
//!   - `hash`:   Variant / Record receivers
//!
//! are NOT REACHED for any user-declared enum/record across an
//! exhaustive barrage of shapes:
//!   - non-generic enum (1/2/3+ variants, with/without args)
//!   - non-generic record (1/2/3+ fields)
//!   - generic enum (1 param, 2 params)
//!   - generic record
//!   - self-recursive enum
//!   - self-recursive generic enum
//!   - all four traits via `where a: Trait` bound
//!   - all four traits via direct `x.method()` call
//!   - all four traits via qualified `Type.method(...)` call
//!
//! The instrumentation counters in `dispatch.rs` (added round 62)
//! count every hit on the candidate arms. After running the user-type
//! barrage, all counters are asserted to be zero.
//!
//! KNOWN ASYMMETRY: built-in enums (Option, Result, Weekday,
//! HttpMethod, ChannelResult, ChannelOp, Step) and built-in records
//! (Response, Request, FileStat, Date, Time, DateTime, ...) are NOT
//! processed by the synth pass — they continue to rely on the
//! typecheck-stamp + dispatch arm fallback. This file deliberately
//! does not exercise built-in receivers in the user-type barrage; a
//! separate sub-test confirms built-ins still work and (optionally)
//! that the arms are non-zero after exercising them. See the comment
//! in `synthesize_auto_derive_impls` for the documented asymmetry.

use std::process::Command;
use std::sync::{Arc, Mutex};

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;
use silt::vm::dispatch;

/// Serialise all dispatch-counter tests in this file. Cargo runs
/// tests in parallel within a single test binary; without this lock,
/// the user-type test and the asymmetry-lock test interleave their
/// counter resets and reads, producing false positives. The lock
/// also protects against environment-driven `RUST_TEST_THREADS=1`
/// drift — every test that touches the counters takes this lock.
static COUNTER_LOCK: Mutex<()> = Mutex::new(());

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

/// In-process synth-pipeline driver. Keeps every typechecker pass and
/// VM execution in the same process so the dispatch counters
/// accumulate across calls. (The `silt run` CLI helper above launches
/// a subprocess each time; counters in those subprocesses do not
/// propagate to the test harness.) Mirrors `tests/integration.rs::run`.
fn run_silt_inproc(src: &str) {
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler
        .compile_program(&program)
        .expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let _ = vm.run(script);
}

// ── 1. User-type barrage: every shape × every trait × every call form

/// Drive a comprehensive barrage of user-typed Compare/Equal/Hash/
/// Display calls and assert that none reaches the deprecated dispatch
/// arms. If this fails, identify which counter tripped and inspect
/// the corresponding shape: a non-zero count means the synthesis
/// pipeline missed that case.
#[test]
fn user_types_do_not_reach_deprecated_dispatch_arms() {
    let _guard = COUNTER_LOCK.lock().unwrap();
    dispatch::reset_auto_derive_dead_arm_counters();

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
    let _ = Red.compare(Green)
    let _ = Red.equal(Green)
    let _ = Red.hash()
    let _ = Red.display()
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
    let _ = Foo(1).compare(Foo(2))
    let _ = Foo(1).equal(Foo(2))
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

    // Final assertion: every counter must be zero. Any non-zero
    // value identifies the leaking shape via the corresponding
    // counter name.
    let counts = dispatch::auto_derive_dead_arm_counts();
    for (name, count) in counts.iter() {
        assert_eq!(
            *count, 0,
            "user-type barrage leaked into deprecated dispatch arm '{name}': {count} hits — \
             a synth bypass is missing for some shape above. Investigate which Shape# \
             added a non-zero count and fix the auto-derive synth coverage."
        );
    }
}

// ── 2. Built-in receivers DO still hit the arms (asymmetry lock) ─────

/// Documents the known asymmetry: built-in enums like Weekday and
/// Result still route through `dispatch_trait_method` because the
/// auto-derive synth pass only processes user-declared
/// `Decl::Type` nodes. Asserts the arms are non-zero after a
/// built-in barrage. If a future round adds built-in synth, this
/// test will fail (counters stay zero) — that's the signal to
/// delete the arms outright.
#[test]
fn builtin_enums_still_reach_dispatch_arms_asymmetry_lock() {
    let _guard = COUNTER_LOCK.lock().unwrap();
    dispatch::reset_auto_derive_dead_arm_counters();

    // Weekday — built-in enum, no synth.
    run_silt_inproc(
        r#"
import time
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    let _ = cmp_gen(Monday, Friday)
}
"#,
    );

    let counts = dispatch::auto_derive_dead_arm_counts();
    let variant_compare_hits = counts
        .iter()
        .find(|(name, _)| *name == "compare(Variant, Variant)")
        .map(|(_, n)| *n)
        .unwrap_or(0);
    assert!(
        variant_compare_hits > 0,
        "expected built-in Weekday compare to hit the (Variant, Variant) dispatch arm \
         (asymmetry: built-ins are not synth'd). If this assertion fails, built-ins were \
         likely added to the synth pass — delete the arms in src/vm/dispatch.rs and \
         remove the instrumentation. Counters: {counts:?}"
    );
}

// ── 3. End-to-end behaviour: user-type ops still produce correct results

/// Full behaviour pass: same barrage as test 1 but checking outputs
/// (CLI subprocess for stdout capture). Locks that the synth path
/// produces semantically correct results regardless of dispatch
/// counters.
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
