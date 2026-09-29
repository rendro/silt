//! Round 104 regression lock: where-bound trait obligations must keep
//! the CROSS-SLOT linkage of NON-LINEAR impl self types.
//!
//! `type Pair(a) = (a, a)` with `trait Show2 for Pair(a)` registers the
//! alias-expanded self type `(Var a', Var a')` — the SAME binder in both
//! slots. The round-102/103 self-type-args check in
//! `verify_trait_obligation` (src/typechecker/mod.rs) compared obligated
//! args against impl args with the STATELESS per-pair
//! `trait_arg_compatible`, whose `(_, Var)` arm defers each slot
//! independently — so an obligated `(Int, Fn(Int) -> Int)` deferred slot
//! 0 against `a'` AND slot 1 against `a'` and satisfied the bound,
//! losing the constraint that both slots are the same `a'`. Verified
//! pre-fix: the `describe((1, f))` repro below passed `silt check`
//! (exit 0) and died at the round-95 interp Display execution gate on
//! the Fn in slot 1. Direct receiver dispatch (`(1, f).show2()`)
//! correctly rejected — the unify of `(Int, Fn)` against `(a', a')`
//! fails at the second slot — proving the where-bound path was the
//! inconsistent one. The same hole existed for every alias-expanded
//! non-linear shape, e.g. `type Square(a) = Map(a, a)` (a direct
//! `trait T for Map(a, a)` is a parse error — impl-target binders must
//! be distinct — so aliases are the one route to non-linear self
//! types).
//!
//! The fix (`impl_self_args_consistent` / `impl_arg_matches_canon` in
//! src/typechecker/mod.rs) threads a binding map across ALL slots: the
//! first obligated type an impl-side `Var` meets binds it, and every
//! re-encounter must be compatible with that binding. Distinct binders
//! keep binding independently (linear impls unchanged), obligated-side
//! `Var`s still defer, and concrete/concrete pairs walk structurally as
//! before. Prior rounds' locks
//! (`round102_alias_impl_self_type_args_where_bound_tests`,
//! `round103_tuple_fn_alias_where_bound_tests`,
//! `round74_parametric_alias_trait_impl_tests`) only cover LINEAR
//! params and must stay green alongside this file.

use std::process::Command;

use silt::typechecker;
use silt::types::Severity;

// ── Helpers ─────────────────────────────────────────────────────────

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Run a silt source program via the `silt run` subprocess and return
/// (stdout, stderr, success). Mirrors the helper in
/// `tests/typecheck/round103_tuple_fn_alias_where_bound_tests.rs`.
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round104_nonlinear_linkage_{label}.silt"));
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

const PAIR_PRELUDE: &str = r#"
type Pair(a) = (a, a)
trait Show2 { fn show2(self) -> String }
trait Show2 for Pair(a) where a: Display {
  fn show2(self) -> String {
    let (x, y) = self
    "{x} and {y}"
  }
}
fn describe(x: p) -> String where p: Show2 { x.show2() }
"#;

// ── BROKEN: (Int, Fn) satisfied the (a, a)-only impl ────────────────

/// The audit repro: `(Int, Fn(Int) -> Int)` must NOT satisfy
/// `where p: Show2` when the only `Show2` impl is for the non-linear
/// `Pair(a) = (a, a)`.
///
/// Pre-fix this passed `silt check` (exit 0) — both slots deferred
/// against the same impl binder independently — and `silt run` died at
/// the runtime Display gate on the Fn in slot 1 (the `a: Display`
/// obligation was registered only at slot 0, so the Fn slot was never
/// verified either).
#[test]
fn pair_alias_where_bound_rejects_cross_slot_fn_mismatch() {
    let src = format!(
        r#"{PAIR_PRELUDE}
fn main() {{
  let f = {{ n -> n + 1 }}
  println(describe((1, f)))
}}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Show2'")),
        "(Int, Fn) obligated against the (a, a)-only impl MUST reject at \
         check time; got: {errs:?}"
    );
    // Directional message: name the one impl that exists — the
    // non-linear `(a, a)` shape renders as `(_, _)`.
    assert!(
        errs.iter()
            .any(|e| e.contains("the only impl is for") && e.contains("(_, _)")),
        "diagnostic should name the '(a, a)' impl (rendered '(_, _)'); \
         got: {errs:?}"
    );

    // Belt-and-braces: `silt run` must not succeed either (pre-fix it
    // reached the interp Display execution gate and died there).
    let (stdout, _stderr, ok) = run_silt_raw("reject_int_fn", &src);
    assert!(
        !ok,
        "silt run must fail for (Int, Fn) against the (a, a)-only impl; \
         it printed: {stdout:?}"
    );
}

/// Linkage, not bounds: `(Int, String)` must ALSO reject — even though
/// Int and String each individually satisfy the impl's `a: Display`
/// bound. Pre-fix this passed check (each slot deferred independently
/// and the slot-0-only `Display` obligation was satisfied by Int), so
/// this case isolates the cross-slot linkage as the thing being
/// enforced, independent of the where-clause walk.
#[test]
fn pair_alias_where_bound_rejects_even_when_both_slots_satisfy_bound() {
    let src = format!(
        r#"{PAIR_PRELUDE}
fn main() {{ println(describe((1, "z"))) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Show2'")),
        "(Int, String) obligated against the (a, a)-only impl MUST reject \
         at check time even though Int and String both satisfy Display; \
         got: {errs:?}"
    );
    let (stdout, _stderr, ok) = run_silt_raw("reject_int_string", &src);
    assert!(
        !ok,
        "silt run must fail for (Int, String) against the (a, a)-only \
         impl; it printed: {stdout:?}"
    );
}

// ── Positive control: the linear instantiation still RUNS ───────────

/// `describe((1, 2))` — both slots the same concrete type — must keep
/// typechecking AND executing end-to-end (prints "1 and 2"). Guards
/// against the linkage fix over-rejecting the legitimate instantiation.
#[test]
fn pair_alias_where_bound_runs_matching_slots() {
    let src = format!(
        r#"{PAIR_PRELUDE}
fn main() {{ println(describe((1, 2))) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.is_empty(),
        "(Int, Int) satisfies the alias-expanded (a, a) impl; got: {errs:?}"
    );
    let (stdout, stderr, ok) = run_silt_raw("accept_int_int", &src);
    assert!(ok, "silt run must succeed; stderr: {stderr}");
    assert_eq!(stdout.trim(), "1 and 2");
}

// ── Same hole on a Map-shaped alias: `Square(a) = Map(a, a)` ────────
//
// (A direct `trait Tag for Map(a, a)` is a PARSE error — the parser
// enforces distinct binders in impl targets — so alias expansion is the
// one route to a non-linear impl self type, for Map exactly as for the
// tuple `Pair`.)

const SQUARE_MAP_PRELUDE: &str = r#"
type Square(a) = Map(a, a)
trait Tag { fn tag(self) -> String }
trait Tag for Square(a) { fn tag(self) -> String = "square" }
fn tag_it(x: t) -> String where t: Tag { x.tag() }
"#;

/// `Map(String, Int)` must NOT satisfy a bound whose only impl is the
/// alias-expanded non-linear `Map(a, a)`. Pre-fix: both slots deferred
/// against the same binder independently, so ANY map satisfied the
/// bound.
#[test]
fn nonlinear_map_impl_where_bound_rejects_mismatched_key_value() {
    let src = format!(
        r#"{SQUARE_MAP_PRELUDE}
fn main() {{ println(tag_it(#{{ "k": 1 }})) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Tag'")),
        "Map(String, Int) obligated against the Map(a, a)-only impl MUST \
         reject at check time; got: {errs:?}"
    );
    let (stdout, _stderr, ok) = run_silt_raw("reject_string_int_map", &src);
    assert!(
        !ok,
        "silt run must fail for Map(String, Int) against the Map(a, a)-only \
         impl; it printed: {stdout:?}"
    );
}

/// ... while `Map(Int, Int)` matches the non-linear impl and RUNS.
#[test]
fn nonlinear_map_impl_where_bound_runs_matching_key_value() {
    let src = format!(
        r#"{SQUARE_MAP_PRELUDE}
fn main() {{ println(tag_it(#{{ 1: 2 }})) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.is_empty(),
        "Map(Int, Int) satisfies the Map(a, a) impl; got: {errs:?}"
    );
    let (stdout, stderr, ok) = run_silt_raw("accept_int_int_map", &src);
    assert!(ok, "silt run must succeed; stderr: {stderr}");
    assert_eq!(stdout.trim(), "square");
}

// ── Positive control: DISTINCT binders stay independent ─────────────

/// `type Two(a, b) = (a, b)` uses two distinct binders — mixed slot
/// types must keep working through the where-bound, guarding against
/// the binding map over-linking DIFFERENT impl-side vars.
#[test]
fn linear_two_param_alias_where_bound_still_accepts_mixed_slots() {
    let src = r#"
type Two(a, b) = (a, b)
trait First { fn first_str(self) -> String }
trait First for Two(a, b) where a: Display {
  fn first_str(self) -> String {
    let (x, _) = self
    "{x}"
  }
}
fn head_of(x: p) -> String where p: First { x.first_str() }
fn main() { println(head_of((7, "seven"))) }
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "distinct binders (a, b) must accept mixed (Int, String); got: {errs:?}"
    );
    let (stdout, stderr, ok) = run_silt_raw("accept_mixed_two", src);
    assert!(ok, "silt run must succeed; stderr: {stderr}");
    assert_eq!(stdout.trim(), "7");
}
