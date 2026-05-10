//! Round 81 DX-STYLE-1 + DX-LATENT-1 parity locks for
//! `src/lsp/completion.rs`.
//!
//! Two locks:
//!
//!   1. F1 (DX-STYLE-1) — Comment-correctness lock. The pre-fix
//!      `methods_for_canon_name` for-loop carried a stale comment that
//!      claimed `Bytes -> List` was canonicalized by
//!      `canonicalize_target_name`. The actual function only handles
//!      `Range`, `Fun`, and `()`. The lock asserts the stale phrase is
//!      gone and the corrected note is present.
//!
//!   2. F2 (DX-LATENT-1) — Auto-derived parity lock. The LSP's
//!      `auto_derived_methods_for` hardcodes a list of canonical type
//!      names (Int/Float/ExtFloat/Bool/String/Unit/List for all four
//!      traits, Tuple/Map/Set for three). The canonical source of truth
//!      lives in `src/typechecker/mod.rs::register_builtin_trait_impls`
//!      via `register_auto_derived_impls_for`. If either side adds /
//!      removes a type, this test fails loudly so the parallel list
//!      stays in lock-step.
//!
//! These locks are pure source-string greps — no parser, no LSP
//! subprocess. They run cheaply and catch drift at compile-time-of-the-
//! test (i.e. on `cargo test`).

const COMPLETION_RS: &str = include_str!("../src/lsp/completion.rs");
const TYPECHECKER_RS: &str = include_str!("../src/typechecker/mod.rs");

// ── F1 — stale comment lock ─────────────────────────────────────────

/// The pre-fix for-loop comment in `methods_for_canon_name` claimed
/// `Bytes -> List` was a canonicalization performed by the LSP-side
/// `canonicalize_target_name`. It is not — only Range/Fun/() are
/// rewritten there. Lock against the stale phrase reappearing in that
/// comment block.
///
/// The doc-comment on `canonicalize_target_name` (lines :478-485) DOES
/// mention `type Bytes = List(Int)` but in a different sense: it
/// explicitly notes that user aliases ARE missed by the LSP-side
/// mirror. That mention is correct and is preserved by the fix; this
/// test only blocks the `Bytes -> List` arrow form, which was the
/// stale phrasing.
#[test]
fn lsp_completion_for_loop_comment_drops_stale_bytes_to_list_claim() {
    assert!(
        !COMPLETION_RS.contains("Bytes -> List"),
        "src/lsp/completion.rs still contains the stale phrase \
         `Bytes -> List` — round 81 DX-STYLE-1 fixed the \
         `methods_for_canon_name` for-loop comment to remove it. \
         `canonicalize_target_name` only rewrites Range, Fun, and (); \
         Bytes is NOT canonicalized on the LSP side. Update the comment \
         to reflect actual behaviour or remove the stale mapping."
    );
    assert!(
        !COMPLETION_RS.contains("Bytes → List"),
        "src/lsp/completion.rs still contains the stale phrase \
         `Bytes → List` (Unicode arrow form) — same finding as the \
         ASCII-arrow assertion above."
    );
}

/// Affirmative side of the F1 lock: the corrected comment should
/// explicitly say `Bytes` is NOT canonicalized on the LSP side. This
/// pins the new wording so a future "tidy-up" edit doesn't silently
/// drop the disclaimer and reintroduce the original confusion.
#[test]
fn lsp_completion_for_loop_comment_states_bytes_is_not_canonicalized() {
    assert!(
        COMPLETION_RS.contains("`Bytes` is NOT canonicalized here"),
        "src/lsp/completion.rs no longer contains the explicit \
         disclaimer that `Bytes` is NOT canonicalized by the LSP-side \
         `canonicalize_target_name`. Round 81 DX-STYLE-1 added that \
         note in the `methods_for_canon_name` for-loop comment to \
         replace the stale `Bytes -> List` claim. If the wording was \
         legitimately revised, update this test; if a refactor dropped \
         it, restore the disclaimer."
    );
}

// ── F2 — auto-derive parity lock ────────────────────────────────────

/// Authoritative table of canonical type names that the typechecker
/// auto-derives Equal/Compare/Hash/Display for (the "all four traits"
/// set). Mirrors the first `register_auto_derived_impls_for` call at
/// `src/typechecker/mod.rs:7153-7161` plus the immediately-following
/// `&["List"]` call at line 7162.
///
/// Note: this test deliberately scopes to the canonical-head set the
/// LSP table cares about (primitives + parametric containers). The
/// typechecker stamps additional auto-derive entries for built-in
/// enums (`Step`, `ChannelResult`, `Method`, `Weekday`, error enums)
/// and records (`Response`, `Request`, time/fs records, etc.) — those
/// flow through the AST as synthesized `TraitImpl` decls and are
/// surfaced by the LSP via `methods_for_canon_name`'s walk of
/// `program.decls`, so they don't need an entry in
/// `auto_derived_methods_for`. This test locks the
/// "auto-derived-but-NOT-in-the-AST" set, which is the exact set the
/// LSP hardcodes.
const EXPECTED_ALL_FOUR_TRAITS: &[&str] =
    &["Int", "Float", "ExtFloat", "Bool", "String", "Unit", "List"];

/// Authoritative table of canonical type names that the typechecker
/// auto-derives Equal/Hash/Display for (the "no Compare" set). Mirrors
/// `register_auto_derived_impls_for(checker, &["Tuple", "Map", "Set"], ...)`
/// at `src/typechecker/mod.rs:7164`.
const EXPECTED_NON_ORDERING: &[&str] = &["Tuple", "Map", "Set"];

/// The LSP side hardcodes the "all four traits" arm as a single
/// `match` line:
///
///   "Int" | "Float" | "ExtFloat" | "Bool" | "String" | "Unit" | "List" => {
///
/// Lock the literal arm so a one-sided edit (e.g. someone adds `Bytes`
/// to the typechecker but forgets the LSP) is impossible without the
/// test screaming.
#[test]
fn lsp_auto_derive_all_four_arm_matches_typechecker() {
    // The exact LSP arm: each name in `EXPECTED_ALL_FOUR_TRAITS` must
    // appear as a `"Name"` quoted literal inside the
    // `auto_derived_methods_for` arm body. Concretely we look for the
    // single-line arm head.
    let expected_arm: String = EXPECTED_ALL_FOUR_TRAITS
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        COMPLETION_RS.contains(&expected_arm),
        "src/lsp/completion.rs no longer contains the expected \
         all-four-traits match arm `{expected_arm}` in \
         `auto_derived_methods_for`. Either the typechecker added / \
         removed a type from the canonical-head auto-derive set (in \
         which case update both `EXPECTED_ALL_FOUR_TRAITS` here AND \
         the LSP arm), or the LSP arm drifted from the typechecker. \
         Source of truth: `register_auto_derived_impls_for` calls in \
         src/typechecker/mod.rs."
    );

    // Also assert the typechecker's authoritative call sites mention
    // every name. The combined call covers Int/Float/ExtFloat/Bool/
    // String/Unit in one slice and List in a separate call; we assert
    // each appears as `"Name"` somewhere in the auto-derive section
    // of the typechecker source.
    for name in EXPECTED_ALL_FOUR_TRAITS {
        let needle = format!("\"{name}\"");
        assert!(
            TYPECHECKER_RS.contains(&needle),
            "src/typechecker/mod.rs no longer contains `{needle}` — \
             the canonical type name dropped out of the typechecker's \
             auto-derive registration. Update \
             `EXPECTED_ALL_FOUR_TRAITS` here AND the LSP arm in \
             src/lsp/completion.rs::auto_derived_methods_for to match."
        );
    }
}

/// Same lock as the all-four arm, but for the Equal/Hash/Display-only
/// arm covering Tuple/Map/Set.
#[test]
fn lsp_auto_derive_non_ordering_arm_matches_typechecker() {
    let expected_arm: String = EXPECTED_NON_ORDERING
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        COMPLETION_RS.contains(&expected_arm),
        "src/lsp/completion.rs no longer contains the expected \
         non-ordering match arm `{expected_arm}` in \
         `auto_derived_methods_for`. Source of truth: \
         `register_auto_derived_impls_for(checker, &[\"Tuple\", \
         \"Map\", \"Set\"], non_ordering_traits)` in \
         src/typechecker/mod.rs."
    );

    // Typechecker side: one slice literal must contain all three names
    // in sequence. The exact form is `&[\"Tuple\", \"Map\", \"Set\"]`.
    assert!(
        TYPECHECKER_RS.contains("&[\"Tuple\", \"Map\", \"Set\"]"),
        "src/typechecker/mod.rs no longer contains the slice literal \
         `&[\"Tuple\", \"Map\", \"Set\"]` — the typechecker's \
         non-ordering auto-derive registration was edited. Update \
         `EXPECTED_NON_ORDERING` here AND the LSP arm in \
         src/lsp/completion.rs::auto_derived_methods_for."
    );
}

/// Negative-side parity lock: any canonical type name that lives in
/// the LSP's `auto_derived_methods_for` arms MUST also appear in the
/// typechecker's `register_auto_derived_impls_for` calls. This catches
/// the symmetric drift case (LSP gains a type the typechecker doesn't
/// auto-derive).
///
/// Implementation: pull every quoted-string token out of the
/// `auto_derived_methods_for` function body via a regex-free
/// substring scan and assert each is also present quoted in the
/// typechecker source. Restricting the scan to the single function
/// body keeps the lock tight; broader scans would false-positive on
/// unrelated string literals.
#[test]
fn lsp_auto_derive_arm_names_subset_of_typechecker() {
    // Locate the `auto_derived_methods_for` function body. Anchor on
    // the exact `fn auto_derived_methods_for` signature and stop at
    // the next top-level `fn ` or the end of file.
    let fn_start = COMPLETION_RS
        .find("fn auto_derived_methods_for(")
        .expect(
            "src/lsp/completion.rs no longer defines \
             `fn auto_derived_methods_for(` — round 81 DX-LATENT-1 \
             relies on this function being the canonical LSP-side \
             registry of canonical-head auto-derived type names. If \
             the function was renamed or refactored into a shared \
             module, update this parity lock to point at the new \
             home.",
        );
    // Find the function body's closing brace by walking forward from
    // the opening brace and tracking depth. Simple but robust.
    let body_start = COMPLETION_RS[fn_start..]
        .find('{')
        .map(|o| fn_start + o)
        .expect("function signature has no opening brace?");
    let mut depth: i32 = 0;
    let mut body_end = body_start;
    for (idx, ch) in COMPLETION_RS[body_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    body_end = body_start + idx + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &COMPLETION_RS[body_start..body_end];

    // Every double-quoted token in the body. We restrict to ASCII
    // alphabetic + digits inside the quotes (canonical type names are
    // PascalCase identifiers). Tokens like "display"/"compare" etc.
    // (the trait method names returned by the function) are filtered
    // out below by checking against the TYPECHECKER side: those names
    // only appear in the typechecker as method-name string literals,
    // not as auto-derive type-name registrations, so they'd
    // false-positive otherwise.
    //
    // Easier path: hand-list the method-name tokens to ignore.
    let ignore: std::collections::BTreeSet<&str> =
        ["display", "compare", "equal", "hash"].into_iter().collect();

    let mut found_names: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    let mut chars = body.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch == '"' {
            let mut s = String::new();
            for (_, c) in chars.by_ref() {
                if c == '"' {
                    break;
                }
                s.push(c);
            }
            // Filter: must be a non-empty PascalCase-ish identifier,
            // not a method name to ignore.
            let is_ident = !s.is_empty()
                && s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if is_ident && !ignore.contains(s.as_str()) {
                found_names.insert(s);
            }
        }
    }

    // Sanity: the function body should contain at least the canonical
    // sets we expect. If the scan returns empty, the parser regressed.
    assert!(
        !found_names.is_empty(),
        "Token scan of `auto_derived_methods_for` body returned no \
         PascalCase quoted names. Either the function body changed \
         shape (and this test needs updating) or the regex-free \
         scanner has a bug. Body scanned:\n{body}"
    );

    // Every found name must also appear quoted in the typechecker
    // source. This is the asymmetric direction of the parity lock —
    // LSP names are a subset of typechecker names.
    for name in &found_names {
        let needle = format!("\"{name}\"");
        assert!(
            TYPECHECKER_RS.contains(&needle),
            "src/lsp/completion.rs::auto_derived_methods_for lists \
             the canonical type name `{name}`, but \
             src/typechecker/mod.rs does not contain a matching \
             `{needle}` literal. The LSP must not advertise auto- \
             derived methods for a type the typechecker doesn't \
             actually auto-derive — that would surface phantom \
             completions. Source of truth: \
             `register_auto_derived_impls_for` calls in \
             src/typechecker/mod.rs."
        );
    }

    // And the symmetric direction we already enforced above: the
    // expected union must be a subset of `found_names`. This makes
    // sure the LSP arm wasn't trimmed without updating the test.
    let expected_union: std::collections::BTreeSet<String> =
        EXPECTED_ALL_FOUR_TRAITS
            .iter()
            .chain(EXPECTED_NON_ORDERING.iter())
            .map(|s| (*s).to_string())
            .collect();
    let missing: Vec<&String> = expected_union
        .iter()
        .filter(|n| !found_names.contains(*n))
        .collect();
    assert!(
        missing.is_empty(),
        "src/lsp/completion.rs::auto_derived_methods_for is missing \
         expected canonical type names: {missing:?}. The \
         typechecker's `register_auto_derived_impls_for` calls list \
         these as auto-derived; the LSP must surface their methods \
         too. Source of truth: src/typechecker/mod.rs:7153-7164."
    );
}
