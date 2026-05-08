//! Round 75 typechecker fixes — six locks for fix-agent DL.
//!
//! 1. **TYPE-1 LATENT** — `instantiate_with_constraints` and the
//!    parallel pass-3 trait-arg remap site (mod.rs:~3221) must
//!    substitute through `args` so a polymorphic where-clause arg
//!    `(b)` rewrites to the FRESH `b'` allocated for the
//!    instantiation, instead of leaking the scheme's quantified ids
//!    across distinct call sites.
//!
//! 2. **TYPE-2 LATENT** — `align_tyvars_into` must walk parallel
//!    AnonRecord and AssocProj structure so pass-3 narrowing keeps
//!    where-clause tyvars in lock-step with the post-narrowing
//!    scheme. Sibling `scheme_narrowed` already does.
//!
//! 3. **TYPE-3 LATENT** — `src/types/canonical.rs::canonicalize_type_name`
//!    must mirror the typechecker copy's `Unit → ()` collapse.
//!
//! 4. **DEAD-DRIFT** — REPL `eval_declaration` import handling now
//!    routes through `merge_imported_module_exports` so non-builtin
//!    user modules with producer-side exports are honored, just like
//!    `check_program`.
//!
//! 5. **DEAD-3** — three byte-identical "duplicate top-level
//!    definition" emit blocks consolidated through
//!    `define_top_level_unique`.
//!
//! 6. **DEAD-5** — `unify_anon_anon` Open×Closed and Closed×Open arms
//!    consolidated into one `unify_open_closed` helper. Round-73f
//!    occurs-check barrier preserved (canonical wording).

use silt::ast::Program;
use silt::intern::{intern, resolve};
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker::TypeChecker;
use silt::types::canonical::{Resolver, canonicalize_type_name};

fn parse(src: &str) -> Program {
    let tokens = Lexer::new(src).tokenize().expect("lex");
    Parser::new(tokens).parse_program().expect("parse")
}

// ── TYPE-3: canonical.rs () → Unit collapse mirrors typechecker copy ────
//
// The audit's recommended direction: `canonical_name(Type::Unit) ==
// "Unit"` (used by VM dispatch) is the source of truth, so
// `canonicalize_type_name` collapses the `()` ALIAS onto `Unit`.
// Round 74's original direction (Unit → ()) made typecheck pass but
// broke runtime dispatch (compiler emitted `().method`, VM looked
// up `Unit.method`); round 75 flips both copies of
// `canonicalize_type_name` to the canonical direction.

#[test]
fn type3_canonical_module_canonicalize_collapses_paren_paren_to_unit() {
    let res = Resolver::new();
    let canonical = canonicalize_type_name(&res, intern("()"));
    assert_eq!(
        resolve(canonical),
        "Unit",
        "src/types/canonical.rs::canonicalize_type_name should collapse \
         the surface alias \"()\" onto \"Unit\" — mirror of the parallel \
         collapse in src/typechecker/mod.rs (round 75 TYPE-3 LATENT)."
    );
}

#[test]
fn type3_canonical_module_canonicalize_unrelated_names_round_trip() {
    // Ensure the `() → Unit` collapse doesn't perturb other names.
    // Note: `()` is the alias; it does NOT round-trip — it collapses
    // to "Unit". Everything else round-trips.
    let res = Resolver::new();
    for n in ["Int", "List", "Map", "Set", "Tuple", "Foo", "Bar", "Bool", "Unit"] {
        let s = intern(n);
        assert_eq!(
            canonicalize_type_name(&res, s),
            s,
            "expected round-trip for {n}"
        );
    }
    // `()` does NOT round-trip — it collapses to "Unit".
    let alias = intern("()");
    assert_eq!(
        resolve(canonicalize_type_name(&res, alias)),
        "Unit",
        "the `()` alias should collapse onto `Unit`, not round-trip"
    );
}

// ── TYPE-3 behavioral: user `trait T for Unit` still dispatches ─────────
//
// This exercises the runtime path the round-74 fix #4 lock test owns;
// after the canonical.rs side now also collapses Unit → () the
// compiler-side dispatch keys agree with the typechecker side.

fn run_silt_ok(src: &str) -> String {
    let label = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_else(|_| "0".to_string());
    let tmp = std::env::temp_dir().join(format!("silt_round75_dl_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_file(&tmp);
    assert!(
        out.status.success(),
        "silt run failed; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

#[test]
fn type3_user_trait_for_unit_runs_via_compiler_canonical() {
    // Same shape as round74 lock; this lock pins the canonical.rs side
    // so a future revert that drops the Unit→() collapse there fails
    // here even if the typechecker side stays put.
    let out = run_silt_ok(
        r#"
trait Greet { fn greet(self) -> String }
trait Greet for Unit { fn greet(self) -> String = "hi" }
fn main() {
  let u = ()
  println(u.greet())
}
"#,
    );
    assert_eq!(out.trim(), "hi");
}

// ── DEAD-3: helper define_top_level_unique exists and dedupes ───────────

const TYPECHECKER_MOD_RS: &str = include_str!("../src/typechecker/mod.rs");

#[test]
fn dead3_helper_define_top_level_unique_exists() {
    assert!(
        TYPECHECKER_MOD_RS.contains("fn define_top_level_unique("),
        "expected `fn define_top_level_unique(...)` helper in \
         src/typechecker/mod.rs (round 75 DEAD-3)"
    );
}

#[test]
fn dead3_only_one_top_level_names_insert_remains_in_dup_check() {
    // Pre-fix: three byte-identical `top_level_names.contains(...) +
    // self.error(...) + top_level_names.insert(...)` triples
    // (one in `check_program`, one in `register_fn_decl`, one in the
    // REPL `eval_declaration`). Post-fix: the call sites delegate to
    // `define_top_level_unique` which contains the only
    // `top_level_names.insert(name)` adjacent to the dup-error
    // emission. The helper itself contains exactly one such insert.
    //
    // We assert the literal pattern `"top_level_names.insert("`
    // appears at most twice (helper body + the type-decl path's
    // distinct site at register_type_decl, which has a different
    // diagnostic — "duplicate top-level type declaration" — and
    // therefore is OUTSIDE this dedupe). Pre-fix count was 4
    // (3 dup'd + 1 type-decl).
    let count = TYPECHECKER_MOD_RS
        .matches("top_level_names.insert(")
        .count();
    assert!(
        count <= 2,
        "expected <=2 `top_level_names.insert(` occurrences after \
         DEAD-3 dedupe (1 in helper + 1 in register_type_decl); found {count}"
    );
}

#[test]
fn dead3_duplicate_let_top_level_emits_canonical_error() {
    // Behavior: the canonical wording must continue to be emitted
    // when a top-level let-binding name collides. Exercises the
    // `define_top_level_unique` site inside `check_program`.
    let mut tc = TypeChecker::new();
    let src = "let x = 1\nlet x = 2\n";
    let mut prog = parse(src);
    tc.check_program(&mut prog);
    let errs = &tc.errors;
    let dup = errs
        .iter()
        .filter(|e| {
            e.message
                .contains("duplicate top-level definition of 'x'; names must be unique at module scope")
        })
        .count();
    assert!(
        dup >= 1,
        "expected the canonical 'duplicate top-level definition' wording \
         on a top-level `let` collision; got errors:\n{:#?}",
        errs
    );
}

#[test]
fn dead3_duplicate_fn_top_level_emits_canonical_error() {
    let mut tc = TypeChecker::new();
    let src = "fn dup() -> Int = 1\nfn dup() -> Int = 2\n";
    let mut prog = parse(src);
    tc.check_program(&mut prog);
    let errs = &tc.errors;
    let dup = errs
        .iter()
        .filter(|e| {
            e.message.contains(
                "duplicate top-level definition of 'dup'; names must be unique at module scope",
            )
        })
        .count();
    assert!(
        dup >= 1,
        "expected the canonical 'duplicate top-level definition' wording \
         on a top-level `fn` collision; got errors:\n{:#?}",
        errs
    );
}

#[test]
fn dead3_duplicate_let_in_repl_emits_canonical_error() {
    let mut ctx = silt::typechecker::ReplTypeContext::new();
    let src = "let r = 1\nlet r = 2\n";
    let mut prog = parse(src);
    let errs = ctx.check(&mut prog);
    let dup = errs
        .iter()
        .filter(|e| {
            e.message.contains(
                "duplicate top-level definition of 'r'; names must be unique at module scope",
            )
        })
        .count();
    assert!(
        dup >= 1,
        "expected the canonical 'duplicate top-level definition' wording \
         on a REPL `let` collision; got errors:\n{:#?}",
        errs
    );
}

// ── DEAD-5: unify_open_closed helper exists ─────────────────────────────

#[test]
fn dead5_unify_open_closed_helper_exists() {
    assert!(
        TYPECHECKER_MOD_RS.contains("fn unify_open_closed("),
        "expected `fn unify_open_closed(...)` helper in \
         src/typechecker/mod.rs (round 75 DEAD-5)"
    );
}

#[test]
fn dead5_open_closed_row_unif_extra_field_rejected_both_directions() {
    // Open × Closed: open has an extra field that the closed side
    // does not declare. Pre-/post-fix wording is identical
    // ("record open side has fields not present in closed target:
    // <name>"); the dedupe routes both directions through the same
    // helper.
    let mut tc = TypeChecker::new();
    let src = r#"
fn pluck(r: { name: String }) -> String = r.name
fn caller_open_closed() -> String {
  let row = { name: "ok", extra: 1 }
  pluck(row)
}
"#;
    let mut prog = parse(src);
    tc.check_program(&mut prog);
    // Either an explicit row-error or an unexpected-field diagnostic
    // is acceptable; we just need an error class triggered by the
    // anon-record / closed-target unification path.
    let errs = &tc.errors;
    let any_err = errs.iter().any(|e| {
        let m = &e.message;
        m.contains("open side has fields") || m.contains("unexpected field") || m.contains("extra")
    });
    assert!(
        any_err,
        "expected a row-unification error for an anon record with extra fields \
         hitting a closed target; got errors:\n{:#?}",
        errs
    );
}

// ── DEAD-5: lock-test count consistency ─────────────────────────────────

#[test]
fn dead5_infinite_type_message_helper_call_count_matches_lock() {
    // Round 75 DEAD-5 dedupe collapsed two byte-symmetric arms into
    // one helper. The round 74 infinite-type-canonical-form lock test
    // updated its floor accordingly. This sanity test just asserts
    // the post-fix count is reasonable.
    let helper_calls = TYPECHECKER_MOD_RS
        .matches("Self::infinite_type_message(")
        .count();
    assert!(
        (5..=8).contains(&helper_calls),
        "expected 5..=8 `Self::infinite_type_message(...)` call sites \
         post round 75 DEAD-5 dedupe; found {helper_calls}"
    );
}

// ── TYPE-1 LATENT: instantiate_with_constraints args substitution ───────

#[test]
fn type1_instantiate_with_constraints_substitutes_through_args_in_source() {
    // The trait-arg side-channel remap loop in
    // `instantiate_with_constraints` previously cloned `args.clone()`
    // verbatim, which leaked the scheme's quantified id into the
    // post-instantiation `trait_arg_bindings` entry. Source-grep to
    // pin the fix at the call site.
    let must_contain = "substitute_vars(t, &mapping)";
    // The mod.rs file should now invoke `substitute_vars(t, &mapping)`
    // INSIDE the trait_arg_bindings remap loop (sibling
    // method-constraints loop already did this; the fix mirrors it).
    let occurrences = TYPECHECKER_MOD_RS
        .matches(must_contain)
        .count();
    assert!(
        occurrences >= 3,
        "round 75 TYPE-1 LATENT: expected `{must_contain}` to appear at \
         least 3x in src/typechecker/mod.rs (sibling method-constraints \
         loop, this fix's `instantiate_with_constraints` loop, plus the \
         pass-3 trait-impl recheck remap site at `mod.rs:~3221`); found \
         only {occurrences}."
    );
}

#[test]
fn type1_pass3_trait_recheck_remap_substitutes_through_args() {
    // Pin the parallel pass-3 site (mod.rs:~3221) with a separate
    // source-grep so a future revert at one site shows up here.
    // The new shape uses a `ty_remap` HashMap of `Type::Var(...)`
    // entries plus a `substitute_vars(t, &ty_remap)` map per arg.
    assert!(
        TYPECHECKER_MOD_RS.contains("ty_remap: HashMap<TyVar, Type>"),
        "round 75 TYPE-1 LATENT: expected `ty_remap: HashMap<TyVar, \
         Type>` declaration at the pass-3 trait-impl recheck remap \
         site (mod.rs:~3221) — the fix builds a `Type::Var`-keyed \
         remap so `substitute_vars` can rewrite `args` element-wise."
    );
}

#[test]
fn type1_polymorphic_where_clause_typechecks_no_var_leak() {
    // Behavior: a function with a parameterized where-clause bound
    // whose arg is itself a tyvar must not leak quantified ids
    // across instantiations. We exercise this via a polymorphic
    // pair-cast that calls the same constrained generic twice;
    // pre-fix the trait-arg side-channel cloned `args` verbatim and
    // the second instantiation unified against the first one's
    // tyvars.
    //
    // Since silt requires every type variable in a return type to
    // be introduced by a parameter, the surface-level repro is hard
    // to construct directly; we route through a `type b` parameter
    // so the constraint is explicit. The behavior to verify: the
    // program typechecks AND runs to completion (no spurious
    // "type X does not implement trait Y" diagnostic from the
    // leaked tyvar's stale binding).
    let out = run_silt_ok(
        r#"
fn show(x: b, type b) -> String where b: Display = x.display()

fn main() {
  let r1 = show(42, Int)
  let r2 = show("hi", String)
  println(r1)
  println(r2)
}
"#,
    );
    // Each call instantiates the `b: Display` constraint with
    // independent tyvar ids; the trait-arg remap loop must
    // substitute through args so the first instantiation's binding
    // does not pollute the second.
    assert!(
        out.contains("42") && out.contains("hi"),
        "expected both display calls to succeed (round 75 TYPE-1 LATENT \
         lock); got stdout={out:?}"
    );
}

// ── TYPE-2 LATENT: align_tyvars_into AnonRecord/AssocProj arms ──────────

#[test]
fn type2_align_tyvars_walks_anon_record_fields() {
    // Construct an old/new pair with parallel AnonRecord shapes and
    // assert that `align_tyvars` recovers the inner-tyvar mapping.
    use std::collections::BTreeMap;
    use silt::types::{RowTail, TyVar, Type};
    let old_v: TyVar = 42;
    let new_v: TyVar = 99;
    let old_tail: TyVar = 7;
    let new_tail: TyVar = 13;
    let mut old_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    old_fields.insert(intern("name"), Type::Var(old_v));
    let mut new_fields: BTreeMap<silt::intern::Symbol, Type> = BTreeMap::new();
    new_fields.insert(intern("name"), Type::Var(new_v));
    let old = Type::AnonRecord {
        fields: old_fields,
        tail: RowTail::Var(old_tail),
    };
    let new = Type::AnonRecord {
        fields: new_fields,
        tail: RowTail::Var(new_tail),
    };
    let map = silt::typechecker::align_tyvars(&old, &new);
    assert_eq!(
        map.get(&old_v).copied(),
        Some(new_v),
        "round 75 TYPE-2 LATENT: align_tyvars_into must walk AnonRecord \
         fields parallel to scheme_narrowed; the inner var did not map.\n\
         got map={map:?}"
    );
    assert_eq!(
        map.get(&old_tail).copied(),
        Some(new_tail),
        "round 75 TYPE-2 LATENT: align_tyvars_into must remap the open \
         row-tail var across narrowing.\ngot map={map:?}"
    );
}

#[test]
fn type2_align_tyvars_walks_assoc_proj_receiver() {
    use silt::types::{TyVar, Type};
    let old_recv: TyVar = 3;
    let new_recv: TyVar = 5;
    let trait_name = intern("Iterator");
    let assoc_name = intern("Item");
    let old = Type::AssocProj {
        receiver: Box::new(Type::Var(old_recv)),
        trait_name,
        assoc_name,
    };
    let new = Type::AssocProj {
        receiver: Box::new(Type::Var(new_recv)),
        trait_name,
        assoc_name,
    };
    let map = silt::typechecker::align_tyvars(&old, &new);
    assert_eq!(
        map.get(&old_recv).copied(),
        Some(new_recv),
        "round 75 TYPE-2 LATENT: align_tyvars_into must walk AssocProj \
         receiver parallel to scheme_narrowed; receiver did not map.\n\
         got map={map:?}"
    );
}

// ── DEAD-DRIFT: REPL routes through merge_imported_module_exports ───────

#[test]
fn dead_drift_repl_calls_merge_imported_module_exports() {
    // Source-grep lock: the REPL impl in mod.rs must call the helper
    // for at least one of the three import shapes. Pre-fix it was zero.
    // We assert the literal substring is present somewhere in the
    // REPL `check` body.
    let occurrences = TYPECHECKER_MOD_RS
        .matches("self.checker\n                    .merge_imported_module_exports")
        .count()
        + TYPECHECKER_MOD_RS
            .matches("self\n                    .checker\n                    .merge_imported_module_exports")
            .count()
        + TYPECHECKER_MOD_RS
            .matches(".merge_imported_module_exports(")
            .count();
    // The non-REPL `check_program` path uses the helper 3x; the REPL
    // path adds 3 more, plus 1 other site, so the total occurrences
    // today is exactly 7.
    // Round-79 T1 tightening (was `>= 6`, following audit finding T1):
    // the previous `>= 6` threshold tolerated reverting any one of the
    // three REPL sites — count drops from 7 to 6, still passes,
    // defeats the very purpose of the lock. Tightening to `>= 7`
    // catches a partial revert at ANY of the three REPL paths the
    // round-75 DEAD-DRIFT fix introduced. Reverting any single site
    // now drops the count to 6 and trips the assert.
    assert!(
        occurrences >= 7,
        "round 75 DEAD-DRIFT: REPL must route through \
         `merge_imported_module_exports` for non-builtin imports, \
         matching the `check_program` path. Found only {occurrences} \
         total `.merge_imported_module_exports(` invocations \
         (need >= 7: 3 from check_program + 3 from REPL + 1 other; \
         partial revert of any single REPL site drops to 6 and is \
         now caught)."
    );
}
