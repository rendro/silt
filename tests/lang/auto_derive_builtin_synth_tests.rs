//! Built-in enum and record auto-derive stamp coverage.
//!
//! The auto-derive synthesis pass (`synthesize_auto_derive_impls` in
//! `src/typechecker/mod.rs`) emits a `TraitImpl` for every built-in enum
//! and record for each policy-permitted built-in trait. These tests read
//! the typechecker's `trait_impl_set` directly. The end-to-end semantic
//! outputs of the synthesized methods live in the golden cases
//! `tests/golden/lang/traits/auto_derive_builtin_synth__*.silt`.

// ── Coverage tests: every supportable built-in has a synth impl
//
// Walk `self.enums` and `self.records` via a typechecker fingerprint
// helper; for every built-in entry, assert the policy-permitted
// trait stamps are present in `trait_impl_set`. The synth pass keys
// off `trait_impl_set`, so a missing stamp implies missing synth.

#[test]
fn every_builtin_enum_has_stamps_for_policy_permitted_traits() {
    use silt::typechecker::__trait_init_fingerprint_check_program;
    let (impls, _) = __trait_init_fingerprint_check_program();
    // Built-in enums that should have all four built-in trait stamps
    // (every variant arg type is Compare/Equal/Hash/Display-able).
    for type_name in [
        "Step",
        "ChannelResult",
        "Method",
        "Weekday",
        "IoError",
        "JsonError",
        "TomlError",
        "ParseError",
        "HttpError",
        "RegexError",
        "PgError",
        "TcpError",
        "TimeError",
        "BytesError",
        "ChannelError",
    ] {
        for trait_name in ["Equal", "Compare", "Hash", "Display"] {
            let key = format!("{trait_name}:{type_name}");
            assert!(
                impls.contains(&key),
                "expected built-in enum stamp {key} in trait_impl_set;\n  present: {:?}",
                impls
                    .iter()
                    .filter(|s| s.ends_with(&format!(":{type_name}")))
                    .collect::<Vec<_>>(),
            );
        }
    }

    // Option/Result: Equal/Hash/Display only (Compare excluded by
    // `non_ordering_traits` — see `tests/cli/trait_init_parity_tests.rs`).
    for type_name in ["Option", "Result"] {
        for trait_name in ["Equal", "Hash", "Display"] {
            let key = format!("{trait_name}:{type_name}");
            assert!(impls.contains(&key), "expected stamp {key}");
        }
        let compare_key = format!("Compare:{type_name}");
        assert!(
            !impls.contains(&compare_key),
            "did not expect stamp {compare_key} (excluded by policy)",
        );
    }
}

#[test]
fn every_builtin_record_has_stamps_for_policy_permitted_traits() {
    use silt::typechecker::__trait_init_fingerprint_check_program;
    let (impls, _) = __trait_init_fingerprint_check_program();
    // time records — all four built-in trait stamps via
    // `register_auto_derived_impls_for(time, &[...], all_auto_traits)`.
    for type_name in ["Instant", "Date", "Time", "DateTime", "Duration"] {
        for trait_name in ["Equal", "Compare", "Hash", "Display"] {
            let key = format!("{trait_name}:{type_name}");
            assert!(
                impls.contains(&key),
                "expected built-in record stamp {key} in trait_impl_set",
            );
        }
    }
    // FileStat — same (via `fs.rs::register_fs_builtins`).
    for trait_name in ["Equal", "Compare", "Hash", "Display"] {
        let key = format!("{trait_name}:FileStat");
        assert!(impls.contains(&key), "expected built-in record stamp {key}",);
    }
    // Response/Request — Equal/Hash/Display only (Map field blocks
    // Compare). Stamped via `register_builtin_trait_impls`.
    for type_name in ["Response", "Request"] {
        for trait_name in ["Equal", "Hash", "Display"] {
            let key = format!("{trait_name}:{type_name}");
            assert!(impls.contains(&key), "expected stamp {key}");
        }
        let compare_key = format!("Compare:{type_name}");
        assert!(
            !impls.contains(&compare_key),
            "did not expect stamp {compare_key} (Map field has no Compare)",
        );
    }
}
