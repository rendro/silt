//! Round-75 dead-code lock (fix-agent L2), DEAD-7:
//! `src/vm/dispatch.rs::Vm::register_builtins` had two byte-identical
//! loops over the prelude and stdlib-error variant registries, collapsed
//! via `Iterator::chain`. The lock checks at the registry layer that the
//! merged loop cannot clobber entries: the two registries have disjoint
//! variant names.

use std::collections::BTreeSet;

/// Behavioural idempotency lock: the merged loop must register every
/// variant the two pre-collapse loops did — no entries lost or doubled.
///
/// We can't peek into `Vm::globals` from an integration test
/// (`pub(crate)`), but we can verify the round-trip at the registry
/// layer: the loop is data-driven from the two `module::*` helpers, so
/// summing their `(variant)` counts gives the exact number of
/// `globals.insert(...)` calls the merged loop performs.
#[test]
fn dead7_registry_count_matches_sum_of_two_registries() {
    let prelude = silt::module::builtin_prelude_enum_variants_with_arity();
    let errors = silt::module::builtin_error_enum_variants_with_arity();

    // Sum of all `(variant_name, arity)` pairs across both registries.
    let prelude_total: usize = prelude.iter().map(|(_, vs)| vs.len()).sum();
    let error_total: usize = errors.iter().map(|(_, vs)| vs.len()).sum();
    let merged_total = prelude_total + error_total;

    assert!(
        merged_total > 0,
        "expected non-empty merged variant registry"
    );

    // Independently verify: every variant name from the two registries
    // is unique (no overlap between prelude and error families). If
    // they overlapped, the merged loop would still register the same
    // count of inserts, but two of those inserts would clobber the same
    // global key. The pre-collapse loops did the same, so this is
    // genuine idempotency parity, but we pin uniqueness too because
    // the silt error enums are deliberately module-prefixed
    // (`IoNotFound`, `JsonSyntax`, …) precisely to avoid clashing with
    // prelude variant names (`Ok`, `None`, …).
    let mut all_names: BTreeSet<&str> = BTreeSet::new();
    for (_, vs) in prelude.iter().chain(errors.iter()) {
        for (name, _arity) in vs.iter() {
            assert!(
                all_names.insert(*name),
                "variant name `{name}` is registered by BOTH the \
                 prelude registry AND the error registry — round-75 \
                 DEAD-7 chain merge would clobber the second \
                 insertion. The two registries must keep disjoint \
                 variant-name sets."
            );
        }
    }
    assert_eq!(
        all_names.len(),
        merged_total,
        "merged registry name-count {} must equal sum-of-arities {} \
         when no name overlaps",
        all_names.len(),
        merged_total
    );

    // Behavioural cross-check: build a `Vm` (which calls
    // `register_builtins` internally) and assert it doesn't panic.
    // This is the closest a public-API integration test can get to
    // exercising the merged loop end-to-end. Pre-collapse had the same
    // behaviour; a regression that breaks the chain merge (e.g. wrong
    // type bound) would surface here as a panic / mis-compile.
    let _vm = silt::vm::Vm::new();
}
