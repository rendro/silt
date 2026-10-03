//! Round-73 BLOAT-1 parity locks for the stdlib typed-error enum
//! registry (`module::builtin_error_enum_variants_with_arity`), which
//! drives both the VM's `<Enum>.message` registration and the
//! typechecker's trait-impl registration.
//!
//! The end-to-end dispatch lock (every error enum's `.message()` works)
//! is the golden case
//! tests/golden/meta/errors/round73_error_enum_registry_parity_tests__every_error_enum_message_dispatches.silt.

use silt::module::builtin_error_enum_variants_with_arity;

#[cfg(not(feature = "postgres"))]
#[test]
fn bloat1_pg_error_skipped_when_feature_off_at_typecheck() {
    use silt::diagnostic::Severity;
    // If PgError were still in the typechecker's trait_impl_set when
    // the postgres feature is off, calling `.message()` on it would
    // typecheck but then crash at runtime (trait_impl_set advertises a
    // trait the runtime can't fulfill). The fix's filter prevents
    // this — but the actual proof is at the construction site (audit
    // gate from Round 64): with the feature off, PgError should not
    // even be a registered enum so the constructor reference fails.
    let src = r#"
import postgres

fn main() {
    let _ = postgres.PgError.PgConnect("nope")
}
"#;
    let (_, errors) = silt::session::testing::analyze_str(src);
    let messages: Vec<String> = errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();
    assert!(
        !messages.is_empty(),
        "with `postgres` feature OFF, PgError must remain unregistered. \
         The Round-73 BLOAT-1 collapse must preserve the cfg-aware \
         filter that the round-64 GAP fix introduced."
    );
}

#[test]
fn bloat1_registry_size_is_load_bearing() {
    // Sanity: the registry currently has 11 entries (matching the
    // pre-fix hand-rolled list). If a new error enum is added the
    // registry grows — and the dispatch loop, being registry-driven,
    // automatically picks it up. This is the round-73 fix's payoff.
    let n = builtin_error_enum_variants_with_arity().len();
    assert!(
        n >= 11,
        "registry must contain all stdlib error enums; got {n}"
    );
}
