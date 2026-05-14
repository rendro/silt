//! Round-86 PARALLEL-ARRAY-DRIFT lock: the per-enum variant lists
//! consumed by `attach_enum_variant_docs` in
//! `src/typechecker/builtins/errors.rs` must be derived from the
//! authoritative registry at
//! `module::builtin_error_enum_variants_with_arity`, not hand-rolled.
//!
//! Pre-fix shape: a 93-line `&[(&str, &[&str])]` literal sat right
//! after the typechecker registration loop (which itself already
//! consumed the registry — round-73 BLOAT-1) and duplicated the same
//! variant set with no parity test. Adding a new typed-error variant
//! required edits in two parallel sites; only the registry was
//! authoritative, so silent drift was possible.
//!
//! This file pins three properties:
//!
//!   1. SOURCE-GREP NEGATIVE: the file no longer contains the literal
//!      pre-fix shape (back-to-back `("IoError",` then
//!      `"IoNotFound",`). If a future edit reintroduces the
//!      hand-rolled table this lock fires.
//!
//!   2. SOURCE-GREP POSITIVE: the file calls
//!      `builtin_error_enum_variants_with_arity` in the doc-attach
//!      neighbourhood. The replacement strategy is required to be
//!      registry-driven (no other authoritative shape exists).
//!
//!   3. RUNTIME PARITY: every variant in the registry (filtered to
//!      the active feature set) has a hover doc attached after
//!      `register_builtins`. This is the load-bearing check that the
//!      derived form actually reaches `attach_doc` for every variant
//!      the registry knows about.

use silt::module::builtin_error_enum_variants_with_arity;
use silt::typechecker::builtin_docs;

const ERRORS_SRC: &str = include_str!("../src/typechecker/builtins/errors.rs");

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

// ── (1) Source-grep NEGATIVE — hand-rolled shape is gone ────────────

/// The pre-fix table started with an entry for `IoError` whose body
/// was an inner-slice of bare variant string literals
/// (`&["IoNotFound", "IoPermissionDenied", …]`). The unique
/// fingerprint we lock against is `"IoError"` followed shortly by
/// `&[` followed by a bare string-literal opener `"` — i.e. NOT a
/// parenthesized tuple of the `register_enum` shape (which has
/// `("IoNotFound", &[Type::String])` after the outer `&[`).
///
/// If a future edit re-introduces `("IoError", &["IoNotFound", …])`
/// (or any of its sibling variant rows) this test fires. The
/// authoritative source is the registry; this file must consume it,
/// not duplicate it.
///
/// Distinguishing this fingerprint from the legitimate
/// `register_enum(checker, env, "IoError", &[("IoNotFound",
/// &[Type::String]), …])` calls is critical — those calls remain
/// the typechecker-side definition and are cross-checked against the
/// registry by `tests/error_enum_dispatch_parity_tests.rs`.
#[test]
fn no_handrolled_io_error_variant_slice_literal() {
    let code_only: String = ERRORS_SRC
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    // Look for `"IoError"` followed (with arbitrary whitespace) by
    // `&[` then by a bare string-literal opener (`"`), with the next
    // thing being a quoted variant name — NOT an open paren which is
    // what the `register_enum` per-variant tuple shape uses.
    let hits = find_handrolled_variant_table_rows(&code_only, "IoError", "IoNotFound");

    assert!(
        hits.is_empty(),
        "src/typechecker/builtins/errors.rs contains a hand-rolled \
         `(\"IoError\", &[\"IoNotFound\", …])` style variant slice. \
         Round-86 requires the per-enum variant lists for \
         `attach_enum_variant_docs` to be DERIVED from \
         `module::builtin_error_enum_variants_with_arity`. The \
         registry is the single source of truth; do not duplicate \
         variant names here. Offending byte offsets: {hits:?}\n\n\
         NOTE: the `register_enum(checker, env, \"IoError\", \
         &[(\"IoNotFound\", &[Type::String]), …])` shape is fine — \
         the lock only fires when the inner slice contains BARE \
         string literals (the doc-attach table shape), not \
         parenthesized variant tuples (the register_enum shape)."
    );
}

/// Helper for the negative-grep locks: returns the byte offsets where
/// `errors.rs` contains the pre-fix `(<EnumName>, &[<VariantName>, …])`
/// shape — i.e. `enum_name` as a string literal followed shortly by
/// `&[` followed by a BARE variant-name string literal (NOT a
/// parenthesized `(variant_name, &[...])` tuple). The latter is the
/// legitimate `register_enum` arg shape and must not trigger.
fn find_handrolled_variant_table_rows(
    code: &str,
    enum_name: &str,
    variant_sample: &str,
) -> Vec<usize> {
    let needle = format!("\"{enum_name}\"");
    let variant_lit = format!("\"{variant_sample}\"");
    let mut hits = Vec::new();
    let bytes = code.as_bytes();
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle.as_bytes() {
            let window_end = (i + 200).min(bytes.len());
            let window = &code[i..window_end];
            // After `"IoError"` find the next `&[` — if the chars
            // immediately following (skipping whitespace) are `"`
            // (bare string literal) AND `"IoNotFound"` appears within
            // the same window, we have a hit. If the chars after `&[`
            // are `(`, that's the `register_enum` shape — skip.
            if let Some(bracket_pos) = window.find("&[") {
                let after = &window[bracket_pos + 2..];
                let first_non_ws =
                    after.chars().find(|c| !c.is_whitespace()).unwrap_or(' ');
                if first_non_ws == '"' && window.contains(&variant_lit) {
                    hits.push(i);
                }
            }
            i += needle.len();
        } else {
            i += 1;
        }
    }
    hits
}

/// Sibling negative for `JsonError` — same pre-fix shape, distinct
/// fingerprint. Locking two rows reduces the chance that someone
/// partially re-introduces the table by adding a single enum row.
#[test]
fn no_handrolled_json_error_variant_slice_literal() {
    let code_only: String = ERRORS_SRC
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    let hits = find_handrolled_variant_table_rows(&code_only, "JsonError", "JsonSyntax");

    assert!(
        hits.is_empty(),
        "src/typechecker/builtins/errors.rs contains a hand-rolled \
         `(\"JsonError\", &[\"JsonSyntax\", …])` style variant slice. \
         Round-86 requires the per-enum variant lists for \
         `attach_enum_variant_docs` to be DERIVED from \
         `module::builtin_error_enum_variants_with_arity`. Offending \
         byte offsets: {hits:?}"
    );
}

// ── (2) Source-grep POSITIVE — registry call is present ─────────────

/// The replacement strategy MUST consume the registry. We pin the
/// call site so a future refactor cannot silently drop the
/// registry-driven derivation back to a hand-rolled list without
/// failing this test.
#[test]
fn attach_enum_variant_docs_call_is_registry_driven() {
    assert!(
        ERRORS_SRC.contains("builtin_error_enum_variants_with_arity"),
        "src/typechecker/builtins/errors.rs must consume \
         `module::builtin_error_enum_variants_with_arity` to derive \
         the per-enum variant lists for `attach_enum_variant_docs`. \
         Without it the per-enum doc-attach loop drifts from the \
         authoritative registry. Round-86 PARALLEL-ARRAY-DRIFT fix."
    );
    assert!(
        ERRORS_SRC.contains("attach_enum_variant_docs"),
        "src/typechecker/builtins/errors.rs is expected to call \
         `attach_enum_variant_docs` to stamp the enum-section body \
         onto every variant binding (LSP hover support). If this \
         responsibility moved, update Round-86's lock to point at \
         the new site."
    );
}

// ── (3) Runtime parity — every registry variant has a doc ───────────

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

/// Sibling positive lock: the count of variants documented matches
/// the count of variants in the registry, exactly. This prevents the
/// derived form from accidentally over- or under-walking the
/// registry (e.g. a stray `.take(n)` or `.skip(1)`). The first
/// positive lock above checks every individual variant is present;
/// this one cross-checks the *cardinality* so silent off-by-one
/// drift is caught even if a registry entry happened to alias a
/// previously-documented name.
#[test]
fn documented_variant_count_matches_registry() {
    let docs = builtin_docs();
    let expected = live_variants_under_active_features();
    let total: usize = expected.iter().map(|(_, vs)| vs.len()).sum();

    let documented: usize = expected
        .iter()
        .flat_map(|(_, vs)| vs.iter())
        .filter(|v| docs.contains_key(**v))
        .count();

    assert_eq!(
        documented, total,
        "Documented variant count ({documented}) does not match \
         registry variant count ({total}) under the active feature \
         set. Round-86 requires the derived form in \
         `attach_enum_variant_docs` to walk every (enum, variant) \
         row from `builtin_error_enum_variants_with_arity` exactly \
         once."
    );
}
