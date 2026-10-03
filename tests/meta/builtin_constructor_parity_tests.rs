//! Round-58 parity lock: every gated enum constructor listed in
//! `src/module.rs::builtin_enum_variants` (the authoritative source of
//! truth for constructors registered by silt's builtin modules) must
//! be recognized by each editor-facing surface that consults builtin
//! lists:
//!
//!   * LSP rename (`src/lsp/rename.rs`) — must reject renames that
//!     target any gated constructor (otherwise `silt rename` corrupts
//!     user programs that call stdlib APIs).
//!   * REPL completion (`src/repl.rs`) — before any input, tab-completion
//!     offers the prelude's constructors and no builtin module's.
//!
//! If this test fails after adding a new gated constructor, the fix is
//! to route the surface through `module::all_builtin_constructor_names`
//! rather than re-adding another hand-rolled list.
//!
//! Before the round-58 fix, three separate hardcoded lists tracked
//! gated constructors — they had diverged, breaking rename and
//! autocompletion for the ~50 typed-error variants (IoNotFound,
//! JsonSyntax, PgConnect, …) plus Recv/Send. Do not loosen this check.

use silt::module::{all_builtin_constructor_names, builtin_enum_variants};

/// Collects every constructor variant (prelude + gated) from the
/// authoritative `builtin_enum_variants` registry. Deduplicated because
/// some names appear in more than one enum (e.g. `Closed`/`Empty` are
/// shared between ChannelResult and ChannelError).
fn all_variants() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = all_builtin_constructor_names().collect();
    out.sort();
    out.dedup();
    out
}

// ─── Authoritative helper sanity ──────────────────────────────────────

#[test]
fn all_builtin_constructor_names_matches_enum_variants_flatten() {
    // Belt-and-braces: the helper must be a pure flatten of
    // `builtin_enum_variants`. If someone changes one without the
    // other the parity lock's notion of "authoritative set" silently
    // drifts.
    let from_helper: Vec<&'static str> = all_builtin_constructor_names().collect();
    let from_enums: Vec<&'static str> = builtin_enum_variants()
        .iter()
        .flat_map(|(_, v)| v.iter().copied())
        .collect();
    assert_eq!(
        from_helper, from_enums,
        "all_builtin_constructor_names must equal flatten(builtin_enum_variants)"
    );
}

#[test]
fn prelude_constructors_present_in_authoritative_set() {
    // Sanity: the helper covers the prelude constructors (the historical
    // "four always-available" set), not just the gated ones.
    let all = all_variants();
    for name in ["Ok", "Err", "Some", "None"] {
        assert!(
            all.contains(&name),
            "expected prelude constructor `{name}` in all_builtin_constructor_names"
        );
    }
}

#[test]
fn gated_constructors_present_in_authoritative_set() {
    // Sanity: the helper covers the gated-error variants that were
    // missing from the hand-rolled lists before round 58.
    let all = all_variants();
    for name in [
        "IoNotFound",
        "JsonSyntax",
        "PgConnect",
        "Recv",
        "Send",
        "Monday",
        "GET",
        "HttpTimeout",
        "BytesInvalidUtf8",
        "ChannelTimeout",
    ] {
        assert!(
            all.contains(&name),
            "expected gated constructor `{name}` in all_builtin_constructor_names"
        );
    }
}

// ─── LSP rename ───────────────────────────────────────────────────────

#[test]
fn lsp_rename_rejects_every_gated_constructor() {
    // Rename must refuse every builtin constructor, gated or not;
    // before round 58 a hand-rolled list covered only about half.
    let accepted: Vec<&'static str> = all_variants()
        .into_iter()
        .filter(|name| silt::lsp::is_user_renameable(name))
        .collect();
    assert!(
        accepted.is_empty(),
        "is_user_renameable accepts builtin constructors: {accepted:?}"
    );
    // `unreachable` was a phantom reserved global, never a builtin.
    assert!(silt::lsp::is_user_renameable("unreachable"));
}

// ─── REPL completion ──────────────────────────────────────────────────

#[test]
fn repl_builtin_names_offer_prelude_constructors_only() {
    // Before any input, <Tab> offers the prelude's constructors and no
    // builtin module's: those are reached through their module once it
    // is imported (`time.Monday`), see
    // tests/cli/round77_repl_enum_completion_tests.rs.
    let names = silt::repl::builtin_names();
    for variant in all_variants() {
        let prelude = matches!(variant, "Ok" | "Err" | "Some" | "None");
        assert_eq!(
            names.iter().any(|n| n == variant),
            prelude,
            "REPL `builtin_names` and the variant `{variant}`: offered iff a prelude name"
        );
    }
}
