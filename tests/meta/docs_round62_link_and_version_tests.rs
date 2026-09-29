//! Round 62 audit lock: doc-link sanity, version freshness, and
//! builtin-count parity for the effect-rows materials.
//!
//! These tests exist because the round-62 audit caught five concrete
//! drifts:
//!
//! - B6: five dangling markdown links pointed at the deleted
//!   `docs/stdlib/` directory (the stdlib reference was inlined into
//!   `src/typechecker/builtins/docs.rs`). Each link must be replaced
//!   with prose that points the user at LSP hover or `silt --help`.
//! - B7: a `println(msg)  -- "hello"` annotation in
//!   `docs/concurrency.md` — `println` on a `Value::String` calls
//!   `display_value` which prints the bare string (no quotes), so the
//!   expectation must be `-- hello` (no quotes).
//! - G2: stale `v0.12 ships` / `for v0.12` prose lingering in v0.13+.
//!   The phases shipped in v0.13; v0.14 keeps the permissive default.
//! - G3: builtin count drift between the proposal (~400 / 401) and
//!   the actual count (388 per commit `0c72f41`).
//! - L9: a non-deterministic concurrency example that printed a
//!   specific receive order — the receive order is a permutation of
//!   the spawn order, not a guarantee.
//!
//! If any of these regress, the test below points at the offending
//! file with a precise message.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_doc(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// B6 lock: none of the five files that previously linked into
/// `docs/stdlib/` may contain `../stdlib/` anywhere. The directory is
/// gone; any link there is broken.
#[test]
fn docs_language_does_not_link_to_deleted_stdlib_dir() {
    let affected = [
        "docs/language/error-handling.md",
        "docs/language/testing.md",
        "docs/language/collections.md",
    ];
    for rel in affected {
        let body = read_doc(rel);
        assert!(
            !body.contains("../stdlib/"),
            "{rel} still contains a link to ../stdlib/ (deleted directory). \
             Replace each occurrence with prose pointing at LSP hover or \
             `silt --help`."
        );
    }
}

/// B7 lock: the `println(msg)  -- "hello"` line in concurrency.md
/// must not include the literal `"hello"` (with quotes) on a `--`
/// comment line — `println(Value::String("hello"))` prints the bare
/// `hello` via `display_value`.
#[test]
fn concurrency_md_println_does_not_quote_string_output() {
    let body = read_doc("docs/concurrency.md");
    let mut hits: Vec<(usize, &str)> = Vec::new();
    for (idx, line) in body.lines().enumerate() {
        let trimmed = line.trim_start();
        // We're looking for `-- "hello"` style comments inside the
        // rendezvous example. Any `--` comment line that pairs a bare
        // double-quoted string after the `--` is a quoted-output
        // annotation; that's the bug.
        if trimmed.starts_with("--") && trimmed.contains("\"hello\"") {
            hits.push((idx + 1, line));
        }
    }
    assert!(
        hits.is_empty(),
        "docs/concurrency.md still has quoted-string println annotations: \
         {hits:?}. `println(value::String)` prints the bare string."
    );
}

/// G2 lock: the effect-rows materials must not present-tense ship a
/// v0.12-only world. v0.14 is the current crate version; the phases
/// shipped in v0.13 and the default is still permissive in v0.14.
#[test]
fn effect_rows_docs_use_v0_13_or_later_for_phase_a_to_d() {
    let migration = read_doc("docs/strict-effects-migration.md");
    let proposal = read_doc("docs/proposals/effect-rows.md");

    // The exact stale phrases the audit flagged. Each must be gone.
    let banned_pairs = [
        (
            "docs/strict-effects-migration.md",
            &migration,
            "v0.12 ships",
        ),
        (
            "docs/strict-effects-migration.md",
            &migration,
            "across all of v0.12 without the flag",
        ),
        (
            "docs/strict-effects-migration.md",
            &migration,
            "for\nv0.12 the choice stays with you",
        ),
        (
            "docs/proposals/effect-rows.md",
            &proposal,
            "implemented in v0.12 (Phases A",
        ),
        (
            "docs/proposals/effect-rows.md",
            &proposal,
            "Default flips in v0.13 or v1.0",
        ),
    ];
    for (path, body, banned) in banned_pairs {
        assert!(
            !body.contains(banned),
            "{path} still contains stale prose `{banned}`. Reword to \
             reflect that v0.14 keeps the permissive default."
        );
    }

    // Round-75 strengthening: also forbid the round-74 misclaim that
    // the phases shipped in v0.12. Verified against git tags: v0.12
    // (`4bb2e9d`) was tagged BEFORE phase A landed (`768c74d`); v0.13
    // (`9e533cf`) is the first release that includes all four phases.
    let v0_12_banned = [
        ("docs/strict-effects-migration.md", &migration),
        ("docs/proposals/effect-rows.md", &proposal),
    ];
    for (path, body) in v0_12_banned {
        assert!(
            !body.contains("Phase D of the effect-row tracking proposal in v0.12"),
            "{path} still claims Phase D shipped in v0.12. \
             The phases shipped in v0.13 (see git tag `9e533cf`)."
        );
        assert!(
            !body.contains("Phases A→D shipped in v0.12"),
            "{path} still claims Phases A→D shipped in v0.12. \
             The phases shipped in v0.13 (see git tag `9e533cf`)."
        );
    }
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

/// L9 lock: the spawn-work-join example in concurrency.md must
/// acknowledge that receive order is a permutation of spawn order,
/// not a fixed `10, 20, 30` sequence. The fix-author's choice was
/// prose-only ("in some order" + "permutation"); we lock both
/// keywords so a future cleanup can swap one phrasing for the other
/// as long as the disclaimer survives.
#[test]
fn concurrency_md_spawn_work_join_disclaims_receive_order() {
    let body = read_doc("docs/concurrency.md");
    let needle = "results: 10, 20, 30";
    let pos = body
        .find(needle)
        .unwrap_or_else(|| panic!("docs/concurrency.md no longer contains `{needle}`"));
    // Take a 600-byte window starting at the match — enough to cover
    // the surrounding `--` comment lines.
    let end = (pos + 600).min(body.len());
    let window = &body[pos..end];
    let has_disclaimer = window.contains("in some order") || window.contains("permutation");
    assert!(
        has_disclaimer,
        "docs/concurrency.md spawn-work-join example near `{needle}` \
         no longer notes that the receive order is non-deterministic. \
         Add `in some order` or `permutation` to the surrounding prose."
    );
}

/// Belt-and-braces check: a fast scan over every `.md` under `docs/`
/// catching any markdown link of the form `](../stdlib/...)`. The
/// round-62 audit listed five exact files but a future drift might
/// reintroduce the link elsewhere.
#[test]
fn no_doc_anywhere_links_to_deleted_stdlib_dir() {
    let mut offenders: Vec<String> = Vec::new();
    walk_md(&repo_root().join("docs"), &mut |path, body| {
        for (idx, line) in body.lines().enumerate() {
            if line.contains("](../stdlib/") || line.contains("](./stdlib/") {
                offenders.push(format!("{}:{}: {}", path.display(), idx + 1, line));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "found markdown links to the deleted docs/stdlib/ directory:\n  {}",
        offenders.join("\n  ")
    );
}

fn walk_md(dir: &Path, visit: &mut dyn FnMut(&Path, &str)) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_md(&path, visit);
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            if let Ok(body) = fs::read_to_string(&path) {
                visit(&path, &body);
            }
        }
    }
}
