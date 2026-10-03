//! Round-64 audit follow-up: parity locks across the typed-error
//! registries.
//!
//! Two issues from the round-64 audit:
//!
//!   * GAP — `PgError`/`TcpError` were unconditionally registered by
//!     the typechecker even though their VM dispatch arms are gated
//!     behind the `postgres`/`tcp` cargo features. A build like
//!         cargo run --no-default-features --features "repl,http,tcp"
//!     would let `PgError.PgConnect("nope")` typecheck and then crash
//!     at runtime with `unknown builtin namespace: PgError`.
//!
//!   * DUP — the typed-error variant set lived in three independent
//!     registries (`src/module.rs`, `src/typechecker/builtins/errors.rs`,
//!     and `src/vm/dispatch.rs`). The dispatch-side list is now
//!     data-driven from `module::builtin_error_enum_variants_with_arity`;
//!     the typechecker's registration is checked against it by
//!     behaviour below.

use silt::diagnostic::Severity;
use silt::module::{builtin_enum_variants, builtin_error_enum_variants_with_arity};

// ── Finding 1 — feature-gate lock ────────────────────────────────────

/// When the `postgres` cargo feature is OFF, the typechecker must NOT
/// register `PgError` as an enum. Constructing `PgError.PgConnect(...)`
/// must therefore be rejected at typecheck time, not at runtime.
///
/// This is gated `#[cfg(not(feature = "postgres"))]` because the
/// happy-path is exactly: feature off → typecheck rejects.
#[cfg(not(feature = "postgres"))]
#[test]
fn pg_error_typecheck_rejects_when_postgres_feature_off() {
    let src = r#"
import postgres

fn main() {
    let _ = postgres.PgError.PgConnect("nope")
}
"#;
    let (_, errors) = silt::session::testing::analyze_str(&src);
    let messages: Vec<String> = errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();
    assert!(
        !messages.is_empty(),
        "with `postgres` feature OFF, the typechecker must reject \
         `PgError.PgConnect(...)` instead of letting it through to \
         runtime where it would crash with `unknown builtin \
         namespace: PgError`. Got no errors."
    );
}

/// Mirror lock for `TcpError`. The audit-driver default test profile
/// usually enables `tcp` (round-64 dev profile is `repl,http,tcp`), so
/// an unconditional `cfg(not(feature = "tcp"))` guard would skip in
/// every routine run. We keep the test gated on `not(feature = "tcp")`
/// for build correctness but the spec is identical to PgError.
#[cfg(not(feature = "tcp"))]
#[test]
fn tcp_error_typecheck_rejects_when_tcp_feature_off() {
    let src = r#"
import tcp

fn main() {
    let _ = tcp.TcpError.TcpConnect("nope")
}
"#;
    let (_, errors) = silt::session::testing::analyze_str(&src);
    let messages: Vec<String> = errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();
    assert!(
        !messages.is_empty(),
        "with `tcp` feature OFF, the typechecker must reject \
         `TcpError.TcpConnect(...)` instead of letting it through to \
         runtime."
    );
}

// ── Finding 3 — variant/arity parity ─────────────────────────────────

/// Source-level lock: the variant set in
/// `module::builtin_enum_variants` (names only) must be a perfect
/// subset of `builtin_error_enum_variants_with_arity` for every
/// stdlib error enum. Catches the simplest form of drift — adding a
/// variant in one helper and forgetting the other.
#[test]
fn module_helpers_agree_on_error_enum_variant_names() {
    // Collect (enum_name -> variants) from each helper.
    let with_arity = builtin_error_enum_variants_with_arity();
    let names_only = builtin_enum_variants();

    let mut mismatches: Vec<String> = Vec::new();
    for (enum_name, arity_variants) in with_arity {
        let arity_names: Vec<&str> = arity_variants.iter().map(|(n, _)| *n).collect();
        let names = names_only
            .iter()
            .find(|(e, _)| e == enum_name)
            .map(|(_, vs)| vs.to_vec());
        match names {
            Some(names) => {
                if names != arity_names {
                    mismatches.push(format!(
                        "{enum_name}: names-only={names:?} vs with-arity={arity_names:?}"
                    ));
                }
            }
            None => {
                mismatches.push(format!(
                    "{enum_name}: present in builtin_error_enum_variants_with_arity \
                     but missing from builtin_enum_variants"
                ));
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "module.rs helpers disagree on stdlib error enum variants:\n  - {}",
        mismatches.join("\n  - ")
    );
}

/// The typechecker's registration of every stdlib error enum must
/// agree with `builtin_error_enum_variants_with_arity` on the variant
/// names and arities. Probed by behaviour: for each enum, an exhaustive
/// match with one arm per registry variant (`Name(_, _)` for arity 2)
/// and no wildcard must typecheck. A missing or renamed variant is an
/// undefined constructor, a wrong arity is a field-count error, and an
/// extra typechecker-only variant makes the match non-exhaustive.
#[test]
fn typechecker_error_enums_match_arity_registry() {
    let mut src = String::new();
    let mut imports = std::collections::BTreeSet::new();
    for (enum_name, variants) in builtin_error_enum_variants_with_arity() {
        // Feature-gated enums are registered only when the feature is on.
        if (*enum_name == "PgError" && !cfg!(feature = "postgres"))
            || (*enum_name == "TcpError" && !cfg!(feature = "tcp"))
        {
            continue;
        }
        // Each enum is reached through the builtin module declaring it.
        let module = silt::module::builtin_type_module(enum_name)
            .expect("every stdlib error enum belongs to a module");
        imports.insert(module);
        src.push_str(&format!(
            "fn probe_{}(e: {module}.{enum_name}) -> Int {{\n  match e {{\n",
            enum_name.to_lowercase()
        ));
        for (variant, arity) in variants.iter() {
            let pattern = if *arity == 0 {
                format!("{module}.{variant}")
            } else {
                format!("{module}.{variant}({})", vec!["_"; *arity].join(", "))
            };
            src.push_str(&format!("    {pattern} -> 0\n"));
        }
        src.push_str("  }\n}\n");
    }
    src.push_str("fn main() { () }\n");
    let src = imports
        .iter()
        .map(|m| format!("import {m}\n"))
        .collect::<String>()
        + &src;

    let errors: Vec<String> = silt::session::testing::analyze_str(&src)
        .1
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect();
    assert!(
        errors.is_empty(),
        "src/typechecker/builtins/errors.rs disagrees with \
         module::builtin_error_enum_variants_with_arity:\n  - {}\n\
         probe program:\n{src}",
        errors.join("\n  - ")
    );
}
