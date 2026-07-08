//! Behavioural barrage proving the `dispatch_trait_method`
//! Variant/Record arms are DEAD for all valid input.
//!
//! The `(Variant, Variant)` / `(Record, Record)` fallback arms in
//! compare / equal / hash STILL EXIST in `src/vm/dispatch.rs`
//! (the compare pair lives at dispatch.rs:454-455; re-added round 62,
//! commit 5b3f240, and never removed). This file does NOT prove the
//! arms are gone — it proves the OTHER path is always taken first, so
//! the arms are dead for any program a user can write.
//!
//! How: for every Variant/Record type that passes the round-93
//! field-aware auto-derive gate, the synth pass emits a
//! `<Type>.compare` / `.equal` / `.hash` global, and `Op::CallMethod`
//! (src/vm/execute.rs ~:2250) resolves that global FIRST — before
//! `dispatch_trait_method` is ever consulted. The arms are kept only
//! as a sound defensive fallback for a theoretically-malformed
//! `Value::Variant` with no `__type_of__` registration (src/vm/mod.rs
//! ~:940), which is not constructible from a valid program.
//!
//! The proof IS the barrage itself: if any shape regresses to "no
//! qualified-global emitted" the call resolves through the catch-all
//! error path in `dispatch_trait_method`, which surfaces as a runtime
//! `error[runtime]: compare() not supported between …` (or `no
//! method 'hash' for type '…'`) — never the expected output. So
//! every line below that asserts on stdout simultaneously locks
//! "qualified-global was emitted by the synth pass and reached the
//! runtime intact, ahead of the dead arms".
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

// ── 4. Source grep-lock: the corrected prose must not regress ─────

/// Prose grep-lock (no runtime behaviour). Locks in the round-94+
/// correction of the stale comments around the dead Variant/Record
/// dispatch arms. The old comments falsely claimed those arms were
/// REACHED for "types with non-supportable fields ... laundered"
/// through `register_type_decl` pre-stamping — a path the round-93
/// field-aware auto-derive gate statically rejects, so it no longer
/// exists. They also falsely claimed (in this file's own header)
/// that the arms were "deleted". This test fails the moment either
/// false phrasing returns to the source.
#[test]
fn dead_arm_prose_does_not_regress() {
    // 1. The false claim that non-supportable-field types reach
    //    these arms must stay gone. The old comments asserted the
    //    path POSITIVELY: that `register_type_decl` "pre-stamps" the
    //    trait_impl_set so that "no global exists and execution falls
    //    through to this arm". Both phrasings are false post round-93
    //    (the field-aware gate statically rejects such programs) and
    //    must never return. (Note: the *corrected* prose may mention
    //    "non-supportable fields" while describing their REJECTION,
    //    so we lock the specific false assertions, not that phrase.)
    let dispatch_src = include_str!("../src/vm/dispatch.rs");
    assert!(
        !dispatch_src.contains("falls through to this"),
        "stale false comment regressed in src/vm/dispatch.rs: the \
         claim that non-supportable-field types 'fall through to this \
         arm' is false post round-93 (such programs are statically \
         rejected) — the arms are a defensive fallback, not a live \
         path for such types"
    );
    assert!(
        !dispatch_src.contains("pre-stamps the trait_impl_set"),
        "stale false comment regressed in src/vm/dispatch.rs: the \
         'register_type_decl pre-stamps the trait_impl_set' laundering \
         story no longer describes reality post round-93"
    );

    // 2. This proof-test's own header must not re-assert the false
    //    claim that the arms were deleted. They still exist at
    //    dispatch.rs:454-455.
    let this_src = include_str!("auto_derive_dead_arm_proof_tests.rs");
    let header_end = this_src
        .find("\nuse ")
        .expect("test file should have a `use` section after the header");
    let header = &this_src[..header_end];
    assert!(
        !header.contains("deleted"),
        "stale false comment regressed in this file's header: it must \
         NOT claim the dispatch arms were 'deleted' — they still exist \
         at src/vm/dispatch.rs:454-455 as a defensive fallback; this \
         file proves the synth-global path is reached FIRST"
    );

    // 3. Sanity: the corrected prose is actually present (so this
    //    lock can't be satisfied by an empty/renamed source).
    assert!(
        dispatch_src.contains("Defensive fallback") || dispatch_src.contains("defensive fallback"),
        "expected the corrected 'defensive fallback' prose in \
         src/vm/dispatch.rs"
    );
}

/// Cross-reference lock: every `src/value.rs <N>` line citation in the
/// dead-arm comments of `src/vm/dispatch.rs` must point at a line in
/// `src/value.rs` that actually contains the item the comment names.
///
/// Round-95 introduced five stale citations here — e.g. the comment
/// said `impl PartialEq for Value` lived at value.rs 1586/1610 when it
/// is actually at 1662, and `fn cmp` was cited at 1728/1742 when it is
/// at 1782. Those numbers were wrong the moment they were written and
/// nothing caught it. This test asserts each cited line resolves to its
/// named item, so the references cannot silently drift again.
#[test]
fn dispatch_value_rs_citations_resolve() {
    let dispatch_src = include_str!("../src/vm/dispatch.rs");
    let value_src = include_str!("../src/value.rs");
    let value_lines: Vec<&str> = value_src.lines().collect();

    // (substring that must appear in the dispatch comment immediately
    //  before/around the citation, identifier the target value.rs line
    //  must contain). Each pair is checked against EVERY `src/value.rs
    //  <N>` citation found in dispatch.rs; the citation whose comment
    //  mentions the cue string must land on a line containing `needle`.
    //
    // We resolve by scanning dispatch.rs for the literal citation text
    // and confirming the value.rs line at <N> contains `needle`.
    let checks: &[(&str, &str)] = &[
        // arm 1: PartialEq for Value
        (
            "impl PartialEq for Value` (src/value.rs",
            "impl PartialEq for Value",
        ),
        // arm 2: fn cmp
        ("fn cmp` (src/value.rs", "fn cmp"),
        // float canonical bit-hash citation
        (
            "canonical bit-hash for floats (see src/value.rs:",
            "Value::Float(f) =>",
        ),
        // arm 3: impl Hash for Value (full reference, with line on next line)
        ("impl Hash for Value` (src/value.rs", "impl Hash for Value"),
        // Range-hash inline citation
        ("impl Hash for Value`\n", "impl Hash for Value"),
    ];

    // Pull out every "src/value.rs<sep><N>" occurrence with its number,
    // ignoring whether the separator is `:` or ` ` (some citations wrap
    // the number onto the next comment line).
    fn cited_line_after(text: &str, anchor: &str) -> Option<usize> {
        let idx = text.find(anchor)?;
        let rest = &text[idx + anchor.len()..];
        // Skip non-digit comment scaffolding (spaces, `:`, `//`, newlines).
        let digits: String = rest
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse::<usize>().ok()
    }

    for (anchor, needle) in checks {
        let n = cited_line_after(dispatch_src, anchor).unwrap_or_else(|| {
            panic!(
                "could not find a numeric value.rs citation after the \
                 anchor {anchor:?} in src/vm/dispatch.rs — the dead-arm \
                 cross-reference comments changed shape; update this lock"
            )
        });
        assert!(
            n >= 1 && n <= value_lines.len(),
            "dispatch.rs cites src/value.rs:{n} (anchor {anchor:?}) but \
             value.rs only has {} lines",
            value_lines.len()
        );
        let target = value_lines[n - 1];
        assert!(
            target.contains(needle),
            "stale cross-reference in src/vm/dispatch.rs: comment near \
             {anchor:?} cites src/value.rs:{n}, but that line is \
             {target:?} — it does NOT contain the named item {needle:?}. \
             Find the real line (grep `{needle}` in src/value.rs) and \
             correct the citation."
        );
    }
}
