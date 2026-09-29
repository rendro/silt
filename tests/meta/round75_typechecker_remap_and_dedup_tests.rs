//! Round 75 typechecker fixes — the locks that need internal API.
//!
//! - **TYPE-2 LATENT** — `align_tyvars_into` must walk parallel
//!   AnonRecord and AssocProj structure so pass-3 narrowing keeps
//!   where-clause tyvars in lock-step with the post-narrowing scheme.
//! - **TYPE-3 LATENT** — `src/types/canonical.rs::canonicalize_type_name`
//!   collapses the `()` alias onto `Unit`.
//! - **DEAD-3** — the REPL's duplicate top-level definition check emits
//!   the canonical wording.
//!
//! The behavioural locks of this round (Unit trait dispatch, duplicate
//! top-level `let`/`fn`, open/closed row unification, where-clause
//! instantiation) are golden cases named
//! `round75_typechecker_remap_and_dedup_tests__*` under tests/golden/meta/.

use silt::ast::Program;
use silt::intern::{intern, resolve};
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::types::canonical::{Resolver, canonicalize_type_name};

fn parse(src: &str) -> Program {
    let tokens = Lexer::new(src).tokenize().expect("lex");
    Parser::new(tokens).parse_program().expect("parse")
}

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
    for n in [
        "Int", "List", "Map", "Set", "Tuple", "Foo", "Bar", "Bool", "Unit",
    ] {
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

#[test]
fn type2_align_tyvars_walks_anon_record_fields() {
    // Construct an old/new pair with parallel AnonRecord shapes and
    // assert that `align_tyvars` recovers the inner-tyvar mapping.
    use silt::types::{RowTail, TyVar, Type};
    use std::collections::BTreeMap;
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
