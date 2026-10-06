//! Locks for the derive policy of the built-in traits: which built-in
//! types `register_builtin_trait_impls` gives which trait impls.
//!
//! The fingerprint function used here is `#[doc(hidden)]` on the crate —
//! it exists only so the test can reach into the otherwise `pub(super)`
//! `trait_impl_set` state.

use silt::typechecker::__trait_init_fingerprint_check_program;

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
    // Round 75 TYPE-3 flipped the Unit canonical direction from
    // `Unit → ()` to `() → Unit`, aligning with the VM dispatch
    // oracle. The trait_impl_set keys now use "Unit" not "()".
    for type_name in ["Int", "Float", "Bool", "String", "Unit", "List"] {
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
    // Map and Set have no order. Option and Result have no `compare`
    // method entry (they are ordered by their structure, like any enum).
    // A tuple is ordered part by part and has Compare.
    let (impls, _) = __trait_init_fingerprint_check_program();
    assert!(impls.contains("Compare:Tuple"));
    for type_name in ["Map", "Set", "Option", "Result"] {
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
