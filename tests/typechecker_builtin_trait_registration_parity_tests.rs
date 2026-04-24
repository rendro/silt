//! Round 61 dead-code lock.
//!
//! `register_builtin_trait_impls` used to inline four near-identical
//! TraitInfo construction blocks for Display/Compare/Equal/Hash. Each
//! ~18-line block differed only in (trait_name, method_name, arity,
//! return_type); every other field was defaulted. Round 61 collapsed
//! them to a single `register_builtin_trait_decl(...)` helper called
//! four times.
//!
//! This test locks the semantic no-op of that refactor. It must pass
//! BOTH before and after — if the pre-refactor blocks had diverged in
//! a field that the helper abstracts away, this test would fail and
//! the divergence would surface before the collapse papered over it.
//!
//! The fingerprint function is `#[doc(hidden)]` on the crate — it
//! exists only so this test can reach into the otherwise crate-private
//! `TraitInfo` fields.

use silt::typechecker::__builtin_trait_registration_fingerprint;

#[test]
fn builtin_trait_registration_covers_exactly_four_names() {
    let fp = __builtin_trait_registration_fingerprint();
    let names: Vec<String> = fp.iter().map(|e| e.0.clone()).collect();
    assert_eq!(
        names,
        vec![
            "Display".to_string(),
            "Compare".to_string(),
            "Equal".to_string(),
            "Hash".to_string(),
        ],
        "expected exactly these four built-in trait declarations in order"
    );
}

#[test]
fn builtin_trait_registration_matches_expected_shapes() {
    // Pre-refactor shape, read directly from mod.rs:3397-3474 before
    // the collapse:
    //   Display: method `display`, arity 1, return String
    //   Compare: method `compare`, arity 2, return Int
    //   Equal:   method `equal`,   arity 2, return Bool
    //   Hash:    method `hash`,    arity 1, return Int
    let fp = __builtin_trait_registration_fingerprint();
    let expected: Vec<(&str, &str, usize, &str)> = vec![
        ("Display", "display", 1, "String"),
        ("Compare", "compare", 2, "Int"),
        ("Equal", "equal", 2, "Bool"),
        ("Hash", "hash", 1, "Int"),
    ];
    assert_eq!(
        fp.len(),
        expected.len(),
        "fingerprint length mismatch: {fp:?}"
    );
    for (got, want) in fp.iter().zip(expected.iter()) {
        let (g_name, g_method, g_arity, g_ret, _, _, _, _, _) = got;
        let (w_name, w_method, w_arity, w_ret) = want;
        assert_eq!(g_name, w_name, "trait name mismatch at entry: {got:?}");
        assert_eq!(
            g_method, w_method,
            "method name mismatch for {w_name}: got {g_method}, want {w_method}"
        );
        assert_eq!(
            g_arity, w_arity,
            "arity mismatch for {w_name}: got {g_arity}, want {w_arity}"
        );
        assert_eq!(
            g_ret, w_ret,
            "return type mismatch for {w_name}: got {g_ret}, want {w_ret}"
        );
    }
}

#[test]
fn builtin_trait_registration_fields_are_all_empty_or_default() {
    // The pre-refactor blocks all wrote:
    //   params: Vec::new(),
    //   param_var_ids: Vec::new(),
    //   supertraits: Vec::new(),
    //   supertrait_args: Vec::new(),
    //   param_where_clauses: Vec::new(),
    //   default_method_bodies: HashMap::new(),
    // If the round-61 helper ever starts populating any of these for
    // a trait that the pre-refactor code left empty, this test fires.
    let fp = __builtin_trait_registration_fingerprint();
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
        assert_eq!(
            *supertrait_args_count, 0,
            "{name}: supertrait_args should be empty"
        );
        assert_eq!(
            *default_bodies_count, 0,
            "{name}: default_method_bodies should be empty"
        );
        assert_eq!(*params_count, 0, "{name}: params should be empty");
        assert_eq!(*supertraits_count, 0, "{name}: supertraits should be empty");
        assert_eq!(
            *param_where_clauses_count, 0,
            "{name}: param_where_clauses should be empty"
        );
    }
}
