//! Round-86 PARALLEL-ARRAY-DRIFT lock: the per-enum variant lists
//! consumed by `attach_enum_variant_docs` in
//! `src/typechecker/builtins/errors.rs` are derived from the
//! authoritative registry at
//! `module::builtin_error_enum_variants_with_arity`. The runtime check
//! below walks the registry and requires a hover doc for every variant
//! under the active feature set.

use silt::module::builtin_error_enum_variants_with_arity;
use silt::typechecker::builtin_docs;

/// Variants from the registry that should be live under the active
/// cargo-feature set. `PgError`/`TcpError` are gated; everything else
/// is unconditionally registered.
fn live_variants_under_active_features() -> Vec<(&'static str, Vec<&'static str>)> {
    builtin_error_enum_variants_with_arity()
        .iter()
        .filter(|(_name, _variants)| {
            #[cfg(not(feature = "postgres"))]
            if *_name == "PgError" {
                return false;
            }
            #[cfg(not(feature = "tcp"))]
            if *_name == "TcpError" {
                return false;
            }
            true
        })
        .map(|(name, variants)| (*name, variants.iter().map(|(v, _)| *v).collect()))
        .collect()
}

/// After `register_builtins`, every variant in the registry (filtered
/// by active features) has a markdown doc attached, so LSP hover on
/// `IoNotFound` etc. renders the IoError section's variant table.
/// This is the load-bearing positive lock: source-grep alone cannot
/// catch the case where the call site exists but skips variants.
#[test]
fn every_registry_variant_has_attached_doc() {
    let docs = builtin_docs();
    let expected = live_variants_under_active_features();

    let mut missing: Vec<String> = Vec::new();
    for (enum_name, variants) in &expected {
        for v in variants {
            if !docs.contains_key(*v) {
                missing.push(format!("{enum_name}::{v}"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "Builtin error variants registered by the typechecker have no \
         hover-doc attached. The `attach_enum_variant_docs` call in \
         `src/typechecker/builtins/errors.rs` must walk every variant \
         from `module::builtin_error_enum_variants_with_arity`. \
         Missing docs: {missing:?}"
    );
}
