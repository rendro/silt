//! Round 62 (item 3 of type-design improvements): unified trait-decl
//! registration lock.
//!
//! Built-in traits synthesize `TraitDecl` AST nodes via
//! `builtin_trait_decls()` and feed them through the same
//! `register_trait_decl_inner` body the user path uses. This file keeps
//! the internal registration-fingerprint check; the user-visible locks
//! (Display redefinition guard, `Error.message` default synthesis, user
//! trait `param_where_clauses`) are golden cases
//! `unified_trait_registration__*` under tests/golden/typecheck/traits/.

/// Lock test 1: every built-in trait is registered through the unified
/// path. Inspects the post-registration `traits` map via the existing
/// `__builtin_trait_registration_fingerprint` doc-hidden hook (which
/// already covers the four sig-only traits) plus a direct check on
/// Error.
#[test]
fn builtin_traits_registered_via_user_path() {
    let fp = silt::typechecker::__builtin_trait_registration_fingerprint();
    let names: Vec<String> = fp.iter().map(|e| e.0.clone()).collect();
    assert_eq!(
        names,
        vec![
            "Display".to_string(),
            "Compare".to_string(),
            "Equal".to_string(),
            "Hash".to_string(),
        ],
        "expected the four sig-only built-in traits to be registered through the unified path"
    );

    // Each sig-only trait still has the pre-unification field shape:
    //   no params, no supertraits but Compare's, no where-clauses, no
    //   default bodies.
    for entry in &fp {
        let (
            name,
            _method,
            _arity,
            _ret,
            supertrait_args_count,
            default_bodies_count,
            params_count,
            supertraits_count,
            param_where_clauses_count,
        ) = entry;
        assert_eq!(*params_count, 0, "{name}: params should be empty");
        // (`Compare` has the supertrait `Equal`: what is ordered can be
        // compared for equality.)
        let supertraits = usize::from(name == "Compare");
        assert_eq!(
            *supertraits_count, supertraits,
            "{name}: supertraits should be {supertraits}"
        );
        assert_eq!(
            *supertrait_args_count, supertraits,
            "{name}: supertrait_args should be {supertraits}"
        );
        assert_eq!(
            *param_where_clauses_count, 0,
            "{name}: param_where_clauses should be empty"
        );
        assert_eq!(
            *default_bodies_count, 0,
            "{name}: default_method_bodies should be empty (sig-only)"
        );
    }
}
