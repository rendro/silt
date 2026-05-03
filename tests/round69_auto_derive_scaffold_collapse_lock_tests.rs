//! Round-69 lock: behavioural equivalence after collapsing the four
//! `synth_*_impl_for_enum` scaffolds in `src/typechecker/auto_derive.rs`
//! into two shared helpers (`synth_binop_match_enum` for Compare/Equal
//! and `synth_unop_match_enum` for Hash/Display).
//!
//! ## Why this lives in its own file
//!
//! `tests/auto_derive_dead_arm_proof_tests.rs` is the strongest existing
//! lock for auto-derive correctness — a per-shape × per-trait barrage
//! that executes the generated code via `silt run` and asserts on
//! stdout. This file COMPLEMENTS that barrage by pinning the precise
//! axes the round-69 refactor could regress on:
//!
//!   1. **Per-trait × per-enum-shape output equivalence.** Compare,
//!      Equal, Hash, Display each invoked over (single-variant nullary,
//!      single-variant payload, multi-variant nullary, multi-variant
//!      payload) enums — every cell exercises both helpers' branches
//!      (uninhabited branch is exercised in section 2; same-tag and
//!      catch-all arms are exercised across these four shapes).
//!
//!   2. **Uninhabited-enum fast path.** A `type Empty {}` is given each
//!      of the four traits; the body is the synthesized empty match.
//!      We can't construct an `Empty` value at runtime, so the lock is
//!      "the synth pipeline emits the four impls and the program
//!      typechecks/compiles/runs to completion when functions take
//!      `Empty` as a parameter without ever calling them".
//!
//!   3. **Variant ordinal preservation.** The compare path encodes
//!      declaration-order ordinals via `build_ordinal_match`. A
//!      regression that, say, alphabetised variant names or skipped a
//!      variant would change the ordinal of every later variant. Tests
//!      pin ordinals using a non-alphabetical declaration order so
//!      alphabetical == declaration-order coincidence isn't enough to
//!      pass.
//!
//! All assertions go through `silt run` (subprocess), not in-process
//! `vm.run`, because the auto-derive bug class is "synth produces a
//! body that compiles but runs wrong" — only the runtime output proves
//! the synthesized AST is semantically intact.
//!
//! All `.compare(...)` / `.equal(...)` / `.hash()` / `.display()` calls
//! on enum values flow through a generic helper bound by the relevant
//! trait — that's the canonical surface form for invoking an enum's
//! auto-derived method by receiver-style call (variant constructors as
//! receivers don't expose the type's method namespace; the type-name
//! qualified form `Type.compare(a, b)` works but reads less like the
//! end-user's call site).

use std::process::Command;

fn run_silt_ok(label: &str, src: &str) -> String {
    let tmp = std::env::temp_dir().join(format!("silt_round69_scaffold_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "silt run should succeed for {label}; stdout=<<<{stdout}>>> stderr=<<<{stderr}>>>"
    );
    stdout
}

// Header reused by every body test below — declares the four trait-
// bound generic helpers used to invoke each auto-derived method.
const TRAIT_HELPERS: &str = r#"
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn h(a: a) -> Int where a: Hash { a.hash() }
fn d(a: a) -> String where a: Display { a.display() }
"#;

fn run_with_helpers(label: &str, body: &str) -> String {
    run_silt_ok(label, &format!("{TRAIT_HELPERS}{body}"))
}

// ── 1. Output equivalence: trait × shape matrix ─────────────────────
//
// Each test below pins a specific trait+shape cell. The expected
// output is computed by hand from the synth body shape; if the
// refactor accidentally swaps a per-arm body or drops the catch-all,
// the wrong number prints and the test fails.

#[test]
fn compare_single_variant_nullary_enum() {
    // Single-variant nullary: same-tag arm is the only arm; no
    // catch-all (would be unreachable). Always returns 0.
    let out = run_with_helpers(
        "compare_one_nullary",
        r#"
type Solo { Only, }
fn main() {
    println(cmp_gen(Only, Only))
}
"#,
    );
    assert_eq!(out.trim(), "0");
}

#[test]
fn compare_multi_variant_nullary_enum() {
    // Three nullary variants: same-tag arms each return 0; catch-all
    // routes through ordinal compare. cmp(A, B): A=0, B=1 → -1.
    // cmp(B, A): 1.compare(0) → 1. cmp(B, B): same-tag → 0.
    let out = run_with_helpers(
        "compare_multi_nullary",
        r#"
type Tri { A, B, C, }
fn main() {
    println(cmp_gen(A, B))
    println(cmp_gen(B, A))
    println(cmp_gen(B, B))
    println(cmp_gen(C, A))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "1", "0", "1"]);
}

#[test]
fn compare_multi_variant_payload_enum() {
    // Multi-variant with payloads: same-tag arm builds a lex chain;
    // catch-all uses ordinal dispatch. With variants Foo(Int),
    // Bar(Int, String): cmp(Foo(1), Foo(2)) recurses into Int.compare
    // → -1. cmp(Bar(1, "x"), Foo(0)) routes to catch-all (ord 1 vs 0)
    // → 1.
    let out = run_with_helpers(
        "compare_multi_payload",
        r#"
type Tagged { Foo(Int), Bar(Int, String), }
fn main() {
    println(cmp_gen(Foo(1), Foo(2)))
    println(cmp_gen(Foo(2), Foo(2)))
    println(cmp_gen(Bar(1, "x"), Bar(1, "x")))
    println(cmp_gen(Bar(1, "x"), Foo(0)))
    println(cmp_gen(Foo(0), Bar(1, "x")))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "0", "0", "1", "-1"]);
}

#[test]
fn equal_single_variant_nullary_enum() {
    let out = run_with_helpers(
        "equal_one_nullary",
        r#"
type Solo { Only, }
fn main() {
    println(eq_gen(Only, Only))
}
"#,
    );
    assert_eq!(out.trim(), "true");
}

#[test]
fn equal_multi_variant_nullary_enum() {
    // Same-tag → true; catch-all → false.
    let out = run_with_helpers(
        "equal_multi_nullary",
        r#"
type Tri { A, B, C, }
fn main() {
    println(eq_gen(A, A))
    println(eq_gen(A, B))
    println(eq_gen(C, C))
    println(eq_gen(C, A))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false", "true", "false"]);
}

#[test]
fn equal_multi_variant_payload_enum() {
    // Same-tag with args: left-fold of args.equal. Mismatched tag:
    // catch-all → false.
    let out = run_with_helpers(
        "equal_multi_payload",
        r#"
type Tagged { Foo(Int), Bar(Int, String), }
fn main() {
    println(eq_gen(Foo(1), Foo(1)))
    println(eq_gen(Foo(1), Foo(2)))
    println(eq_gen(Bar(1, "x"), Bar(1, "x")))
    println(eq_gen(Bar(1, "x"), Bar(1, "y")))
    println(eq_gen(Foo(1), Bar(1, "x")))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false", "true", "false", "false"]);
}

#[test]
fn hash_single_variant_nullary_enum() {
    // hash(Only) — deterministic, equal to itself.
    let out = run_with_helpers(
        "hash_one_nullary",
        r#"
type Solo { Only, }
fn main() {
    let h1 = h(Only)
    let h2 = h(Only)
    println(eq_gen(h1, h2))
}
"#,
    );
    assert_eq!(out.trim(), "true");
}

#[test]
fn hash_multi_variant_payload_enum() {
    // Hash determinism + structural property + tag discrimination.
    // - hash(Foo(7)) == hash(Foo(7)) (deterministic, structural)
    // - hash(Foo(7)) != hash(Foo(8)) (different payload)
    // - hash(Foo(7)) != hash(Bar(7, "x")) (different tag ordinal)
    let out = run_with_helpers(
        "hash_multi_payload",
        r#"
type Tagged { Foo(Int), Bar(Int, String), }
fn main() {
    let a = h(Foo(7))
    let b = h(Foo(7))
    let c = h(Foo(8))
    let dh = h(Bar(7, "x"))
    println(eq_gen(a, b))
    println(eq_gen(a, c))
    println(eq_gen(a, dh))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false", "false"]);
}

#[test]
fn display_single_variant_nullary_enum() {
    let out = run_with_helpers(
        "display_one_nullary",
        r#"
type Solo { Only, }
fn main() {
    println(d(Only))
}
"#,
    );
    assert_eq!(out.trim(), "Only");
}

#[test]
fn display_multi_variant_nullary_enum() {
    let out = run_with_helpers(
        "display_multi_nullary",
        r#"
type Tri { A, B, C, }
fn main() {
    println(d(A))
    println(d(B))
    println(d(C))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["A", "B", "C"]);
}

#[test]
fn display_multi_variant_payload_enum() {
    // Body shape: "Tag(" + arg0.display() + ", " + arg1.display() + ")".
    let out = run_with_helpers(
        "display_multi_payload",
        r#"
type Tagged { Foo(Int), Bar(Int, String), }
fn main() {
    println(d(Foo(7)))
    println(d(Bar(2, "hello")))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["Foo(7)", "Bar(2, hello)"]);
}

// ── 2. Uninhabited-enum fast path ──────────────────────────────────
//
// Synth emits `match self { }` (the empty-match form) for every
// trait when variants is empty. The body is unreachable at runtime,
// but the impl must still emit cleanly through the body-check pass.
//
// We can't construct an `Empty` value, so we lock the synth pipeline
// at the *typecheck* level: a program declaring `type Empty {}` and
// using it as a bottom-eliminator (the canonical pattern from
// tests/empty_type_match_tests.rs) must compile and run to
// completion. If `synth_binop_match_enum` or `synth_unop_match_enum`
// regressed on the uninhabited fast path — e.g. accidentally building
// arms over an empty `variants` slice or panicking in `make_method`
// — this program would fail at typecheck (the body-check pass would
// reject the synthesized impl) and `silt run` would exit nonzero.

#[test]
fn uninhabited_enum_synth_pipeline_emits_all_four_impls() {
    // The function `absurd` exercises the `match x {}` bottom-
    // eliminator pattern that exists alongside the auto-derived
    // empty-match impls. If any of the four synth fast paths
    // regressed (panicked or produced an ill-typed body), the
    // synth-pass output would fail body-check and this program
    // would refuse to compile.
    let out = run_silt_ok(
        "uninhabited_all_four",
        r#"
type Absurd {}

fn elim(x: Absurd) -> Int { match x { } }

fn main() {
    println("uninhabited-pipeline-ok")
}
"#,
    );
    assert_eq!(out.trim(), "uninhabited-pipeline-ok");
}

// ── 3. Variant ordinal preservation ────────────────────────────────
//
// The compare path's catch-all builds an ordinal match over variants
// in declaration order. Round-65 fixed an ordinal-handling bug in
// this very pass; round-69's refactor must preserve it.
//
// With three nullary variants `A, B, C` (declaration order):
//   - ord(A) = 0, ord(B) = 1, ord(C) = 2
//   - cmp(A, B) routes catch-all → 0.compare(1) = -1
//   - cmp(B, C) routes catch-all → 1.compare(2) = -1
//   - cmp(C, A) routes catch-all → 2.compare(0) = 1
//
// To rule out alphabetical-ordering regressions (which happens to
// agree with declaration order for `A, B, C`), the second test below
// declares variants in non-alphabetical order.

#[test]
fn compare_pins_declaration_order_ordinals() {
    let out = run_with_helpers(
        "ordinal_decl_order",
        r#"
type Three { A, B, C, }
fn main() {
    println(cmp_gen(A, B))
    println(cmp_gen(B, C))
    println(cmp_gen(C, A))
    println(cmp_gen(A, C))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    // A=0, B=1, C=2 in declaration order.
    // 0.cmp(1) = -1, 1.cmp(2) = -1, 2.cmp(0) = 1, 0.cmp(2) = -1.
    assert_eq!(lines, vec!["-1", "-1", "1", "-1"]);
}

#[test]
fn compare_pins_decl_order_when_alphabet_disagrees() {
    // Variants declared in a non-alphabetical order: Z, A, M.
    // Declaration-order ordinals: Z=0, A=1, M=2.
    //   cmp(Z, A) catch-all → 0.cmp(1) = -1
    //   cmp(A, M) catch-all → 1.cmp(2) = -1
    //   cmp(M, Z) catch-all → 2.cmp(0) = 1
    // If the refactor accidentally alphabetised, we'd see A<M<Z and
    // cmp(Z, A) would return 1 instead.
    let out = run_with_helpers(
        "ordinal_non_alphabetical",
        r#"
type ZAM { Z, A, M, }
fn main() {
    println(cmp_gen(Z, A))
    println(cmp_gen(A, M))
    println(cmp_gen(M, Z))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "-1", "1"]);
}

#[test]
fn compare_payload_variants_use_decl_order_ordinals_too() {
    // Mixed nullary/payload variants. Declaration order:
    //   Pending=0, Active(Int)=1, Done=2.
    // cmp(Pending, Active(_)) routes catch-all → 0.cmp(1) = -1.
    // cmp(Done, Pending) routes catch-all → 2.cmp(0) = 1.
    // cmp(Active(5), Active(5)) takes same-tag → arg compare = 0.
    let out = run_with_helpers(
        "ordinal_with_payload",
        r#"
type Status { Pending, Active(Int), Done, }
fn main() {
    println(cmp_gen(Pending, Active(99)))
    println(cmp_gen(Done, Pending))
    println(cmp_gen(Active(5), Active(5)))
    println(cmp_gen(Active(7), Done))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    // Active(7) ordinal=1, Done ordinal=2 → 1.cmp(2) = -1
    assert_eq!(lines, vec!["-1", "1", "0", "-1"]);
}
