//! Round 62 audit lock: builtin-count parity for the effect-rows
//! proposal (G3: the proposal's count drifted from the actual count,
//! 388 per commit `0c72f41`).

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_doc(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// G3 lock: the proposal's builtin-count callouts must appear at
/// least three times (Friction line, sweep-ordering line, Phase C
/// line) and must agree with the actual implementation count, sourced
/// dynamically. The bidirectional implementation-vs-doc lock lives in
/// `effect_rows_builtin_count_is_pinned_to_implementation` below; this
/// test is a sanity gate that the callouts haven't been deleted.
///
/// The proposal carries BOTH the all-features count (388) and the
/// default-features count (378), since the test suite runs under both
/// configurations on CI. Each callout site (Friction / sweep-ordering
/// / Phase C) must mention the count for the running feature set in
/// either parametric form (e.g. `388 builtins (378 under default
/// features)`), so we count occurrences of `<actual> builtins` for
/// `actual` = the implementation count under the running features.
#[test]
fn effect_rows_builtin_count_matches_committed_classification() {
    let body = read_doc("docs/proposals/effect-rows.md");
    // Stale numbers seen in earlier audit rounds.
    for stale in ["~400 builtins", "401 builtins"] {
        assert!(
            !body.contains(stale),
            "docs/proposals/effect-rows.md still mentions `{stale}`. \
             Reconcile to the implementation count (see \
             `effect_rows_builtin_count_is_pinned_to_implementation`)."
        );
    }
    let actual = silt::typechecker::iter_builtins_for_effects_audit().len();
    // The doc is expected to carry both the all-features count
    // (388 builtins) and the default-features count (378 under
    // default features) at each callout site. Match either form
    // — `<actual> builtins` for the all-features count or
    // `<actual> under default features` for the default-features
    // count — so the test passes under either feature configuration.
    // We normalize whitespace to a single space so the parametric
    // form matches even when prose-wrapped across a newline.
    let normalized: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let primary_needle = format!("{actual} builtins");
    let parametric_needle = format!("{actual} under default features");
    let occurrences = normalized.matches(primary_needle.as_str()).count()
        + normalized.matches(parametric_needle.as_str()).count();
    assert!(
        occurrences >= 3,
        "docs/proposals/effect-rows.md should mention `{primary_needle}` \
         or `{parametric_needle}` in at least three places (Friction / \
         Stdlib sweep ordering / Phase C). Saw {occurrences}. Either \
         the doc has not been updated to the current implementation \
         count, or the callouts have been removed. Note: the doc is \
         expected to carry both the all-features count (388) and the \
         default-features count (378); each site must mention the \
         count for the running feature set."
    );
}

/// G3 strengthening (round-64 LATENT): the previous test only checks
/// that the doc says "388 builtins" three times. That passes whenever
/// the doc says "388" three times — even if a future stdlib edit
/// brings the implementation's classified-builtin count to, say, 389.
/// This test pins the magic number in the doc to the actual count
/// reported by `silt::typechecker::iter_builtins_for_effects_audit`,
/// which is the source of truth (it walks the same `register_builtins`
/// path the typechecker uses at runtime). Bidirectional:
///   - if the impl drifts (e.g. 389) but the doc no longer mentions
///     the running-features count, fail.
///   - if the doc drifts to a magic number that doesn't match any
///     known feature-set count, fail.
/// The error message points at whichever side is wrong.
///
/// Round-65 LATENT-2: `iter_builtins_for_effects_audit().len()` is
/// feature-flag-sensitive — under default features it returns 378,
/// under `--all-features` it returns 388. CI runs `--all-features`
/// but `cargo test` defaults to default features. The doc therefore
/// carries BOTH numbers (e.g. `388 builtins (378 under default
/// features)`), and this test asserts that AT LEAST ONE mentioned
/// count matches `actual` for the running feature set. A doc-side
/// drift to a number that doesn't correspond to any known feature
/// configuration still fails.
#[test]
fn effect_rows_builtin_count_is_pinned_to_implementation() {
    let body = read_doc("docs/proposals/effect-rows.md");
    let actual = silt::typechecker::iter_builtins_for_effects_audit().len();

    // Extract every `<N> builtins` mention from the proposal. The
    // round-62 audit landed three (Friction / Stdlib sweep ordering /
    // Phase C); round-65 added the parametric `(N under default
    // features)` form for feature-flag-sensitive counts. We assert
    // that AT LEAST ONE mentioned number agrees with the running
    // implementation count.
    // Normalize whitespace to a single space so callouts that wrap
    // across newlines (e.g. `378 under default\nfeatures)`) still
    // match the parametric form.
    let normalized: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut mentioned: Vec<u64> = Vec::new();
    for (idx, _) in normalized.match_indices(" builtins") {
        // Walk backwards from `idx` over ASCII digits.
        let prefix = &normalized[..idx];
        let digit_start = prefix
            .rfind(|c: char| !c.is_ascii_digit())
            .map(|p| p + 1)
            .unwrap_or(0);
        if digit_start == idx {
            // No digits immediately before " builtins" — e.g. prose
            // like "function-typed builtins". Skip.
            continue;
        }
        let num_str = &prefix[digit_start..];
        if let Ok(n) = num_str.parse::<u64>() {
            mentioned.push(n);
        }
    }
    // Also pick up the parametric `(N under default features)` form,
    // where the digit run is followed by ` under default features)`
    // rather than ` builtins`.
    for (idx, _) in normalized.match_indices(" under default features") {
        let prefix = &normalized[..idx];
        let digit_start = prefix
            .rfind(|c: char| !c.is_ascii_digit())
            .map(|p| p + 1)
            .unwrap_or(0);
        if digit_start == idx {
            continue;
        }
        let num_str = &prefix[digit_start..];
        if let Ok(n) = num_str.parse::<u64>() {
            mentioned.push(n);
        }
    }

    assert!(
        !mentioned.is_empty(),
        "docs/proposals/effect-rows.md no longer contains any \
         `<N> builtins` callout. Restore the count callouts (Friction \
         / Stdlib sweep ordering / Phase C) — the implementation \
         currently classifies {actual} function-typed builtins."
    );

    let actual_u64 = actual as u64;

    // Known feature-set counts: today the doc legitimately carries
    // both `388` (--all-features) and `378` (default features). Any
    // mentioned number outside this allowlist is a magic-number drift
    // and must be flagged regardless of the running feature set.
    let known_counts: &[u64] = &[378, 388];
    let unknown: Vec<u64> = mentioned
        .iter()
        .copied()
        .filter(|n| !known_counts.contains(n))
        .collect();
    assert!(
        unknown.is_empty(),
        "docs/proposals/effect-rows.md mentions builtin count(s) \
         {unknown:?} that don't correspond to any known feature-set \
         count ({known_counts:?}). The implementation reports \
         {actual} under the running feature set. Either the doc has \
         a magic-number drift, or the implementation count moved and \
         the `known_counts` allowlist in this test needs updating."
    );

    // The running feature set's count must appear at least once.
    let matches_actual = mentioned.iter().any(|n| *n == actual_u64);
    assert!(
        matches_actual,
        "docs/proposals/effect-rows.md mentions builtin counts \
         {mentioned:?} but none match the implementation count \
         {actual} for the running feature set \
         (`silt::typechecker::iter_builtins_for_effects_audit().len() \
         == {actual}`). The proposal is expected to carry both the \
         all-features count (388) and the default-features count \
         (378) so that `cargo test` and `cargo test --all-features` \
         both pass. Update each `<N> builtins` callout in the proposal \
         (Friction / Stdlib sweep ordering / Phase C) to include the \
         {actual} count."
    );
}
