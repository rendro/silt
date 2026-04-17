//! Parity test for the shared built-in trait init helper.
//!
//! `check_program` (used by `silt check` / `silt run`) and
//! `ReplTypeContext::new` (used by the REPL) must register the EXACT
//! same set of built-in trait impls and auto-derived method entries.
//!
//! Historically each site carried its own copy of the registration
//! logic with a comment "mirrors check_program init" — a classic drift
//! trap. The logic now lives in `register_builtin_trait_impls` and both
//! entrypoints delegate to it. This test locks that invariant: if
//! someone re-introduces a second copy and it drifts, or forgets to
//! call the helper from one entrypoint, this test fails.
//!
//! The two fingerprint functions used here are `#[doc(hidden)]` on the
//! crate — they exist only so the test can reach into the otherwise
//! `pub(super)` `trait_impl_set` / `method_table` state.

use silt::typechecker::{__trait_init_fingerprint_check_program, __trait_init_fingerprint_repl};

#[test]
fn check_program_and_repl_agree_on_trait_impls_for_empty_program() {
    let (check_impls, check_methods) = __trait_init_fingerprint_check_program();
    let (repl_impls, repl_methods) = __trait_init_fingerprint_repl();

    assert_eq!(
        check_impls,
        repl_impls,
        "trait_impl_set drift between check_program and ReplTypeContext::new\n\
         only in check_program: {:?}\n\
         only in repl:          {:?}",
        check_impls.difference(&repl_impls).collect::<Vec<_>>(),
        repl_impls.difference(&check_impls).collect::<Vec<_>>(),
    );

    assert_eq!(
        check_methods,
        repl_methods,
        "method_table drift between check_program and ReplTypeContext::new\n\
         only in check_program: {:?}\n\
         only in repl:          {:?}",
        check_methods.difference(&repl_methods).collect::<Vec<_>>(),
        repl_methods.difference(&check_methods).collect::<Vec<_>>(),
    );
}

#[test]
fn trait_impls_cover_every_builtin_trait_name() {
    // Independent sanity check: every built-in trait name should appear
    // on at least one primitive. If a future edit removes all auto-derived
    // registrations for a trait, this catches it before downstream
    // diagnostics get mysterious.
    let (impls, _) = __trait_init_fingerprint_check_program();
    for trait_name in ["Equal", "Compare", "Hash", "Display"] {
        let has_some = impls
            .iter()
            .any(|s| s.starts_with(&format!("{trait_name}:")));
        assert!(
            has_some,
            "no auto-derived impls found for built-in trait {trait_name}"
        );
    }
}

#[test]
fn primitives_get_all_four_traits() {
    // Lock the derive policy: every primitive should have all four
    // built-in traits registered. If policy shifts, this test makes
    // the change visible.
    let (impls, _) = __trait_init_fingerprint_check_program();
    for type_name in ["Int", "Float", "Bool", "String", "()", "List"] {
        for trait_name in ["Equal", "Compare", "Hash", "Display"] {
            let key = format!("{trait_name}:{type_name}");
            assert!(
                impls.contains(&key),
                "expected {key} in trait_impl_set; present: {:?}",
                impls
                    .iter()
                    .filter(|s| s.ends_with(&format!(":{type_name}")))
                    .collect::<Vec<_>>(),
            );
        }
    }
}

#[test]
fn non_ordering_container_types_lack_compare() {
    // Tuple/Map/Set/Option/Result are explicitly excluded from Compare
    // because the VM can't order them at runtime. If someone adds
    // Compare to these, unification will happily accept programs that
    // panic at runtime — this test pins the exclusion.
    let (impls, _) = __trait_init_fingerprint_check_program();
    for type_name in ["Tuple", "Map", "Set", "Option", "Result"] {
        let key = format!("Compare:{type_name}");
        assert!(
            !impls.contains(&key),
            "did not expect {key} — runtime compare() does not support this type"
        );
        // But Equal/Hash/Display should be there.
        for trait_name in ["Equal", "Hash", "Display"] {
            let key = format!("{trait_name}:{type_name}");
            assert!(impls.contains(&key), "expected {key} in trait_impl_set");
        }
    }
}
