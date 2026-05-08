//! Round 79 typechecker-side audit fixes — TS-B1 (BROKEN), TS-L1 / L2 / L3
//! (LATENT). Each fix ships with a regression lock here so a future
//! refactor that reverts the fix surfaces as a failing test.
//!
//! ── TS-B1 (BROKEN): trait_arg_compatible_canon missing AnonRecord arm ──
//!
//! `trait_arg_compatible_canon` in `src/typechecker/mod.rs:2081` had
//! a `_ => false` catch-all that rejected two structurally-equal
//! `Type::AnonRecord` values. Pre-fix a program with a `where a:
//! Convert({a: Int, b: String})` bound paired with a
//! `Convert({a: Int, b: String}) for Int` impl typechecked-fail
//! ("type 'Int' does not implement trait 'Convert({a: Int, b:
//! String})'") because the bound's anon-record arg was compared with
//! the impl's anon-record arg and the catch-all returned false. The
//! fix adds an explicit `(AnonRecord, AnonRecord)` arm that compares
//! field maps element-wise (recursively via the same comparator) and
//! tails for equality.
//!
//! Behavioural test: build the repro program from the round-79
//! finding and run it through the in-process runner — it must
//! typecheck-OK and produce the integer 7. Pre-fix this would either
//! fail typecheck or fail to run.
//!
//! ── TS-L1 (LATENT): chained alias cycle leaves stale alias entries ──
//!
//! `type A = B; type B = A`: A registered first (B not yet visible)
//! with target `Generic("B")`, B then detected the cycle and refused
//! registration — but A was still in the alias table pretending
//! `A → Generic("B")` was valid. A subsequent `let v: A = 42` then
//! reported the misleading "expected B, got Int" instead of a
//! coherent cycle diagnostic.
//!
//! Fix: when a cycle is detected during `register_type_alias`, walk
//! the cycle chain and unregister every entry on the path (and drop
//! the placeholder entries from `type_aliases` / `type_alias_arity`).
//! Use-sites then see "unknown type 'A'" rather than chasing a
//! half-built alias path.
//!
//! ── TS-L2 (LATENT): apply silently keeps RowTail::Var on drift ──
//!
//! Round 76 LATENT T3 aligned merge direction across `apply` (in
//! `src/typechecker/mod.rs`) and `substitute_vars` (in
//! `src/types/mod.rs`) with `or_insert`, but the diagnostic
//! disposition diverged: substitute_vars panics in debug builds when
//! a row tail var resolves to a non-record concrete type ("genuine
//! drift"); apply silently swallowed the same drift. Round 79
//! adds the same `debug_assert!(false, "row tail var bound to
//! non-record concrete type ...")` to apply's else-branch.
//!
//! ── TS-L3 (LATENT): align_tyvars_into AnonRecord drops mappings ──
//!
//! The previous arm used `of.iter().zip(nf.iter())` and only aligned
//! when zipped pair's keys agreed. Mismatched key sets (e.g.
//! pass-3-narrowed scheme has additional fields) silently dropped
//! mappings for keys present on both sides at mismatched positions.
//! Round 79 walks the new field map by key; for each key present in
//! both, recurse into the type pair. Tail-var alignment unchanged.

use std::collections::HashMap;
use std::time::Duration;

use silt::scheduler::test_support::InProcessRunner;
use silt::typechecker;
use silt::types::{Severity, TyVar, Type};
use silt::value::Value;

// ── Helpers ─────────────────────────────────────────────────────────

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

// ── TS-B1: AnonRecord trait-arg compatibility (behavioural) ─────────

/// Round 79 TS-B1 fix: a `where a: Convert({a: Int, b: String})`
/// bound paired with a matching `Convert({a: Int, b: String}) for
/// Int` impl must typecheck cleanly and run end-to-end. Pre-fix the
/// `trait_arg_compatible_canon` catch-all rejected the AnonRecord
/// pair and the obligation was reported as unsatisfied.
#[test]
fn anon_record_trait_arg_satisfies_obligation_end_to_end() {
    let src = r#"
trait Convert(other) { fn convert(self) -> other }
trait Convert({a: Int, b: String}) for Int {
  fn convert(self) -> {a: Int, b: String} { {a: self, b: "x"} }
}
fn use_it(x: a) -> Int where a: Convert({a: Int, b: String}) {
  let r = x.convert()
  r.a
}
fn main() -> Int { use_it(7) }
"#;

    // First, the typechecker must accept the program (no errors).
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "round 79 TS-B1 fix: anon-record trait arg should satisfy \
         the obligation; got errors: {errs:?}"
    );

    // Belt-and-braces: run the program in-process so we observe the
    // round-trip. The bug class is "typechecker rejects valid code,"
    // and the harness `compile_and_run` discards typechecker errors,
    // so a regression that makes typecheck reject would still let
    // `vm.run` proceed — the explicit `errs.is_empty()` check above
    // is the load-bearing assertion. The runner's role is to confirm
    // the program produces the expected integer 7 when executed.
    let runner = InProcessRunner::new(src.to_string()).with_budget(Duration::from_secs(10));
    let outcome = runner.run_trial();
    assert!(
        !outcome.timed_out,
        "round 79 TS-B1 trial timed out; elapsed={:?}",
        outcome.elapsed
    );
    assert!(
        outcome.error_message.is_none(),
        "round 79 TS-B1: vm.run errored: {:?}",
        outcome.error_message
    );
    // `main` is `() -> Int`. The runner returns `Some(Value::Int(7))`.
    match outcome.result {
        Some(Value::Int(7)) => {}
        other => panic!("round 79 TS-B1: expected main() to return Int(7); got {other:?}"),
    }
}

// ── TS-L1: chained alias cycle produces coherent diagnostic ─────────

/// Round 79 TS-L1 fix: `type A = B; type B = A` must not leave A
/// registered with target `Generic("B")`. The diagnostic surface
/// should be the cycle error plus (at the use-site) "unknown type
/// 'A'" — NOT the pre-fix misleading "expected B, got Int" that
/// followed from the half-built alias path.
#[test]
fn chained_alias_cycle_does_not_leak_misleading_type_mismatch() {
    let src = "
type A = B
type B = A
fn main() -> Int {
  let v: A = 42
  v
}
";
    let errs = type_errors(src);
    // Must report the cycle.
    assert!(
        errs.iter().any(|e| e.contains("forms a cycle")),
        "expected the cycle diagnostic; got: {errs:?}"
    );
    // Must NOT report the misleading post-fix-pre-round-79 wording.
    for e in &errs {
        assert!(
            !e.contains("expected B, got Int"),
            "round 79 TS-L1 regression: misleading mismatch \
             'expected B, got Int' surfaced at use-site even though \
             A is part of an unresolved cycle. Full errors: {errs:?}"
        );
        assert!(
            !e.contains("expected A, got Int"),
            "round 79 TS-L1 regression: misleading mismatch \
             'expected A, got Int' surfaced. Full errors: {errs:?}"
        );
    }
}

// ── TS-L2: apply's row-tail drift detector matches substitute_vars ──

/// Round 79 TS-L2 source-grep lock: `apply` in
/// `src/typechecker/mod.rs` must contain the same `debug_assert!`
/// shape as `substitute_vars` in `src/types/mod.rs` so a row tail var
/// bound to a non-record concrete type panics in debug builds at
/// both walks. A behavioural test would require triggering an
/// unreachable case (the unifier's invariant prevents it); the grep
/// is the meaningful lock here.
#[test]
fn apply_row_tail_drift_detector_matches_substitute_vars() {
    let mod_src = include_str!("../src/typechecker/mod.rs");
    // The exact phrasing of the drift message is shared between
    // `apply` (typechecker/mod.rs) and `substitute_vars`
    // (types/mod.rs). Asserting on the message text catches the
    // round-79 fix regressing in either direction.
    assert!(
        mod_src.contains("row tail var bound to non-record concrete type"),
        "round 79 TS-L2 regression: apply's else-branch should \
         contain the same drift `debug_assert!` as substitute_vars; \
         the text 'row tail var bound to non-record concrete type' \
         was not found in src/typechecker/mod.rs"
    );
    // Also confirm it sits inside a `debug_assert!` (not a comment
    // or a panic): if the assertion form drifts the two walks
    // diverge again.
    let needle = "debug_assert!";
    let drift = "row tail var bound to non-record concrete type";
    let drift_pos = mod_src.find(drift).expect("drift text checked above");
    let scan_start = drift_pos.saturating_sub(200);
    let surrounding = &mod_src[scan_start..drift_pos];
    assert!(
        surrounding.contains(needle),
        "round 79 TS-L2 regression: drift detector exists but is \
         not wrapped in `debug_assert!`; surrounding text was: \
         {surrounding:?}"
    );
}

// ── TS-L3: align_tyvars maps common keys regardless of position ─────

/// Round 79 TS-L3 fix (structural unit test): `align_tyvars` must
/// align by key, not by zipped position. Pre-fix two anon records
/// `{x: α, y: β}` (old) and `{y: γ, z: δ}` (new) zip-aligned on the
/// first slot only when keys happened to match — here `x` vs `y`
/// would skip silently (pre-fix `if on == nn` guard) and `y` vs `z`
/// would also skip, so the shared key `y` (β ↦ γ) was lost.
///
/// `align_tyvars` is `pub fn` (no friend module needed): we exercise
/// it directly with hand-rolled types. Behavioural form would
/// require constructing a where-clause-driven body whose pass-3
/// narrowing produces differing key sets — which the unifier's
/// existing invariants prevent — so the structural form is the
/// load-bearing lock.
#[test]
fn align_tyvars_anon_record_walks_by_key_not_position() {
    use std::collections::BTreeMap;

    use silt::intern::intern;
    use silt::typechecker::align_tyvars;
    use silt::types::RowTail;

    let key_x = intern("x");
    let key_y = intern("y");
    let key_z = intern("z");

    let alpha: TyVar = 1;
    let beta: TyVar = 2;
    let gamma: TyVar = 3;
    let delta: TyVar = 4;

    let mut old_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    old_fields.insert(key_x, Type::Var(alpha));
    old_fields.insert(key_y, Type::Var(beta));

    let mut new_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    new_fields.insert(key_y, Type::Var(gamma));
    new_fields.insert(key_z, Type::Var(delta));

    let old = Type::AnonRecord {
        fields: old_fields,
        tail: RowTail::Closed,
    };
    let new = Type::AnonRecord {
        fields: new_fields,
        tail: RowTail::Closed,
    };

    let map: HashMap<TyVar, TyVar> = align_tyvars(&old, &new);
    // β ↦ γ MUST be present — `y` is the shared key. Pre-fix this
    // would be missing because zip put `x`-vs-`y` in slot 0 and
    // `y`-vs-`z` in slot 1, both of which the `on == nn` guard
    // rejected.
    assert_eq!(
        map.get(&beta).copied(),
        Some(gamma),
        "round 79 TS-L3 regression: shared key 'y' should produce \
         β ↦ γ in align_tyvars output regardless of position; map: \
         {map:?}"
    );
    // α has no match (key `x` only on the old side) — must NOT
    // appear in the map.
    assert!(
        !map.contains_key(&alpha),
        "round 79 TS-L3: α is unique to the old side; align_tyvars \
         should leave it unmapped; map: {map:?}"
    );
}

/// Companion test: when key sets agree but positions differ, the
/// post-fix walk still aligns every shared key. Pre-fix `BTreeMap`
/// happens to iterate in key order, so positions agree and zip
/// works — but a future change to ordering could regress without
/// the by-key walk.
#[test]
fn align_tyvars_anon_record_full_overlap_aligns_all() {
    use std::collections::BTreeMap;

    use silt::intern::intern;
    use silt::typechecker::align_tyvars;
    use silt::types::RowTail;

    let key_a = intern("a");
    let key_b = intern("b");

    let alpha: TyVar = 11;
    let beta: TyVar = 12;
    let gamma: TyVar = 13;
    let delta: TyVar = 14;

    let mut old_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    old_fields.insert(key_a, Type::Var(alpha));
    old_fields.insert(key_b, Type::Var(beta));

    let mut new_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    new_fields.insert(key_a, Type::Var(gamma));
    new_fields.insert(key_b, Type::Var(delta));

    let old = Type::AnonRecord {
        fields: old_fields,
        tail: RowTail::Closed,
    };
    let new = Type::AnonRecord {
        fields: new_fields,
        tail: RowTail::Closed,
    };

    let map: HashMap<TyVar, TyVar> = align_tyvars(&old, &new);
    assert_eq!(map.get(&alpha).copied(), Some(gamma));
    assert_eq!(map.get(&beta).copied(), Some(delta));
}
