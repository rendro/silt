//! Round 73 typechecker-side audit fixes — soundness + perf + dedup.
//!
//! Three fixes from the round 73 audit landed in
//! `src/typechecker/mod.rs`. Each ships with a regression lock here so
//! a future refactor that reverts the fix surfaces as a failing test
//! tied to the original bug's diagnostic shape (or a structural source-
//! grep for the perf / dedup fixes where there's nothing observable to
//! assert on at the typechecker boundary).
//!
//! ── TYPE-1 (BROKEN, soundness): row-poly scheme narrowing ──
//!
//! `fn pluck(r) = r.zzznosuchfield` used to typecheck and crash at
//! runtime. The scheme `forall α, β. α → β` and the body-narrowed
//! scheme `forall γ, ρ. {zzznosuchfield: γ; ρ} → γ` both have
//! `vars.len() == 2`, so the legacy `vars.len() != new_vars.len()`
//! gate at mod.rs:3034 silently skipped narrowing. The deferred field
//! check then ran against the body-pass tyvar (still bound to an open
//! AnonRecord) and reported success even though every concrete call
//! site passes a closed record without that field. Fix: extra
//! structural-narrowing detector (`scheme_narrowed`) at mod.rs:6329.
//!
//! ── TYPE-2 (BROKEN, soundness): AnonRecord trait bypass ──
//!
//! `type_name_for_impl` returned `None` for AnonRecord receivers,
//! making every where-constraint check at inference.rs:1117 / 3204 /
//! 3557 and mod.rs:1916 treat the receiver as "still an unresolved
//! tyvar — defer". The deferred check then dropped silently because no
//! impl could ever be registered for an AnonRecord. Fix: synthetic
//! `<anon>` symbol returned from the AnonRecord arm of
//! `type_name_for_impl` (mod.rs:1888). The leading `<` makes the name
//! impossible to collide with any user-declared type identifier.
//!
//! ── TYPE-4 (LATENT, perf): clone hoisted out of inner loop ──
//!
//! `instantiate_with_constraints` cloned `self.trait_arg_bindings`
//! once per outer iteration. The clone is now hoisted above the outer
//! loop (mod.rs:1814). Pure functional no-op; locked via source-grep
//! that the inner-loop clone is gone.
//!
//! ── BLOAT-1 third site: typechecker error-enum registry ──
//!
//! The stdlib error-enum name list at mod.rs:6798 is now sourced from
//! `module::builtin_error_enum_variants_with_arity()` instead of a
//! hand-rolled `&["IoError", ...]`. Locked via source-grep.

use silt::typechecker;
use silt::types::Severity;

const TYPECHECKER_MOD_SRC: &str = include_str!("../src/typechecker/mod.rs");

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

// ── TYPE-1: row-poly narrowing skip rejected nonexistent field ──────

#[test]
fn type1_row_poly_narrowing_rejects_nonexistent_field_on_closed_record() {
    // `pluck` is unannotated; body inference unifies its parameter
    // with `AnonRecord{zzznosuchfield: β; ρ}` (open row). Without
    // structural narrowing the call site instantiates the un-narrowed
    // scheme `α → β`, unifies α with the closed `{a: Int, b: Int}`,
    // and the deferred field check finds zzznosuchfield via the still-
    // open row of the body-pass tyvar.
    let errs = type_errors(
        r#"
fn pluck(r) = r.zzznosuchfield
fn main() {
    let p = {a: 1, b: 2}
    let _ = pluck(p)
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected a type error rejecting `zzznosuchfield` on the closed anon record, got none"
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("zzznosuchfield") || e.contains("no field")),
        "expected diagnostic mentioning the missing field 'zzznosuchfield' or 'no field', got: {errs:?}"
    );
}

#[test]
fn type1_row_poly_narrowing_keeps_passing_existing_field_call() {
    // Sanity: the narrowing detector must not regress a CORRECT
    // row-poly call. `show_name(p) = p.name` called with a record that
    // has `name` must still typecheck.
    let errs = type_errors(
        r#"
fn show_name(p) = p.name
fn main() {
    let _ = show_name({name: "A", age: 30})
}
"#,
    );
    assert!(
        errs.is_empty(),
        "row-poly call with field present must still typecheck after narrowing fix; got: {errs:?}"
    );
}

// ── TYPE-2: AnonRecord trait bypass ────────────────────────────────

#[test]
fn type2_anon_record_does_not_silently_satisfy_user_trait_constraint() {
    // Pre-fix: `type_name_for_impl` returned None for AnonRecord, so
    // `verify_trait_obligation` at the call site silently accepted the
    // anon-record receiver as "still a polymorphic tyvar — defer". No
    // diagnostic ever fired and the code typechecked despite no impl
    // existing.
    let errs = type_errors(
        r#"
trait MyTrait {
  fn my_ident(self) -> Int
}
trait MyTrait for Int {
  fn my_ident(self) -> Int = self
}
fn use_it(x: a) -> Int where a: MyTrait = x.my_ident()
fn main() {
    let r = {x: 1, y: 2}
    let _ = use_it(r)
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected a where-constraint diagnostic on the anon-record receiver, got none"
    );
    assert!(
        errs.iter().any(|e| {
            e.contains("does not implement trait")
                && e.contains("MyTrait")
        }),
        "expected `does not implement trait 'MyTrait'` for the anon record, got: {errs:?}"
    );
}

// ── TYPE-4: perf — `trait_arg_bindings.clone()` hoisted out of inner loop ──

#[test]
fn type4_instantiate_with_constraints_no_inner_loop_clone() {
    // Source-grep lock: the inner-loop clone pattern that produced the
    // O(M·N) regression must not return. The replacement clones once
    // outside the outer loop into a snapshot.
    assert!(
        !TYPECHECKER_MOD_SRC.contains("self.trait_arg_bindings.clone().iter()"),
        "regression: `self.trait_arg_bindings.clone().iter()` reintroduced inside `instantiate_with_constraints` (TYPE-4)"
    );
    assert!(
        TYPECHECKER_MOD_SRC.contains("trait_arg_bindings_snapshot"),
        "expected the hoisted-clone snapshot variable `trait_arg_bindings_snapshot` to remain in `instantiate_with_constraints`"
    );
}

// ── Fix 4 (BLOAT-1, typechecker site): error-enum names sourced from registry ──

#[test]
fn fix4_typechecker_error_enum_names_sourced_from_registry() {
    // SOURCE-LEVEL: the error-enum name set in the typechecker's
    // built-in trait-impl registration must come from
    // `module::builtin_error_enum_variants_with_arity()`. The previous
    // shape was a literal `&["IoError", "JsonError", ...]` array
    // inline at the `register_auto_derived_impls_for` call.
    assert!(
        TYPECHECKER_MOD_SRC.contains("builtin_error_enum_variants_with_arity"),
        "expected `src/typechecker/mod.rs` to consult the authoritative error-enum registry via `builtin_error_enum_variants_with_arity`"
    );
    // Pin that the hand-rolled list (the old multi-line literal block
    // including all stdlib error enum names back-to-back) no longer
    // appears as a single literal. The audit's specific concern is the
    // `&["IoError", ...]` pattern reappearing.
    let combined_literal = "\"IoError\",\n            \"JsonError\"";
    assert!(
        !TYPECHECKER_MOD_SRC.contains(combined_literal),
        "regression: hand-rolled `[\"IoError\", \"JsonError\", ...]` literal block reintroduced in `src/typechecker/mod.rs` (BLOAT-1 typechecker site)"
    );
}
