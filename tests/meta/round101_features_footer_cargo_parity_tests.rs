//! Round-101 GAP lock: closed-world parity between the Cargo.toml
//! `[features]` table and `enabled_features()` in `src/cli/features.rs`
//! (the `Enabled features:` footer of `silt --help`).
//!
//! `enabled_features()` hand-encodes one `cfg!(feature = "...")` row per
//! user-facing feature. Before this lock, nothing went red when a new
//! `[features]` key was added to Cargo.toml without a matching `cfg!`
//! row — a newly added user-facing feature (e.g. a future `sqlite`)
//! would be silently omitted from the footer, reproducing exactly the
//! drift class the footer exists to prevent. Pre-fix repro: add
//! `sqlite = []` under `[features]` — every existing test stays green;
//! this lock fails.
//!
//! Strategy (source-scan; `toml` is a [dependencies] crate, not a
//! dev-dependency, so integration tests parse the table with a small
//! deterministic line scanner):
//!   * Parse the `[features]` table keys out of `include_str!`'d
//!     Cargo.toml.
//!   * Subtract the documented-internal set (see
//!     `INTERNAL_FEATURES` below — excluding a future feature from the
//!     footer must be a deliberate act of editing that list).
//!   * Assert each remaining key appears as the literal
//!     `cfg!(feature = "<key>")` AND `feats.push("<key>")` in
//!     src/cli/features.rs.
//!   * Conversely, assert every `cfg!(feature = "...")` string and
//!     every `feats.push("...")` string in features.rs is a declared
//!     Cargo feature — a stale row for a deleted/renamed feature also
//!     reds.

const CARGO_TOML: &str = include_str!("../../Cargo.toml");
const FEATURES_SRC: &str = include_str!("../../src/cli/features.rs");

/// Features deliberately NOT shown in the `silt --help` footer.
///
/// * `default` — a feature *set* selector, not a capability; listing it
///   would be noise (its members are listed individually).
/// * `test-hooks` — internal-only: exposes `scheduler::test_hooks` /
///   `scheduler::test_support` for external test binaries. Never a
///   user-facing capability (see the comment above it in Cargo.toml).
///
/// Adding a key here silences the parity lock for that feature. Do it
/// only for genuinely internal features, and say why in this doc.
const INTERNAL_FEATURES: &[&str] = &["default", "test-hooks"];

/// Extract the keys of the `[features]` table from Cargo.toml text.
///
/// Line-based on purpose: Cargo.toml is our own file with one
/// `key = value` per line inside the table. The scan starts after the
/// exact `[features]` header line and stops at the next `[`-prefixed
/// section header. Comment and blank lines are skipped.
fn cargo_feature_keys() -> Vec<String> {
    let mut keys = Vec::new();
    let mut in_features = false;
    for line in CARGO_TOML.lines() {
        let trimmed = line.trim();
        if trimmed == "[features]" {
            in_features = true;
            continue;
        }
        if in_features {
            if trimmed.starts_with('[') {
                break; // next table ([[bench]], [dependencies], ...)
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((key, _rest)) = trimmed.split_once('=') {
                keys.push(key.trim().to_string());
            }
        }
    }
    keys
}

/// Extract every string literal that follows `needle` in code lines of
/// `src`, i.e. for needle `cfg!(feature = "` it returns each feature
/// name quoted there. Comment lines (`//`, `///`, `//!`) are skipped so
/// the `cfg!(feature = "...")` placeholder in the module doc-comment is
/// not mistaken for a row. Panics if a match is not closed by a `"` on
/// the same line (would mean the source drifted from the simple literal
/// form this lock greps for).
fn quoted_strings_after(src: &str, needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in src.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let mut rest = line;
        while let Some(idx) = rest.find(needle) {
            let after = &rest[idx + needle.len()..];
            let end = after
                .find('"')
                .unwrap_or_else(|| panic!("unterminated string after `{needle}` in features.rs"));
            found.push(after[..end].to_string());
            rest = &after[end..];
        }
    }
    found
}

/// Canary: the table parser itself must not silently degrade. If a
/// Cargo.toml reformat ever broke `cargo_feature_keys()`, an empty
/// result would make the parity checks below vacuously pass — so pin
/// a couple of long-lived keys and a minimum count.
#[test]
fn cargo_features_table_parser_sees_known_keys() {
    let keys = cargo_feature_keys();
    for known in ["default", "repl", "lsp", "tcp", "test-hooks"] {
        assert!(
            keys.iter().any(|k| k == known),
            "Cargo.toml [features] parser lost known key `{known}`; parsed: {keys:?}"
        );
    }
    assert!(
        keys.len() >= 10,
        "Cargo.toml [features] parser returned suspiciously few keys: {keys:?}"
    );
}

/// Forward direction: every user-facing Cargo feature must have a
/// literal `cfg!(feature = "<key>")` row AND a `feats.push("<key>")`
/// in src/cli/features.rs. This is the lock that reds when a new
/// `[features]` key lands without a footer row.
#[test]
fn every_user_facing_cargo_feature_has_a_footer_row() {
    let keys = cargo_feature_keys();
    let user_facing: Vec<&String> = keys
        .iter()
        .filter(|k| !INTERNAL_FEATURES.contains(&k.as_str()))
        .collect();
    assert!(
        !user_facing.is_empty(),
        "no user-facing features left after exclusions — exclusion set or parser is wrong"
    );
    for key in user_facing {
        let cfg_row = format!("cfg!(feature = \"{key}\")");
        assert!(
            FEATURES_SRC.contains(&cfg_row),
            "Cargo.toml declares feature `{key}` but src/cli/features.rs has no \
             `{cfg_row}` row — the `silt --help` footer would silently omit it. \
             Add the row to enabled_features(), or (for a genuinely internal \
             feature) add `{key}` to INTERNAL_FEATURES in this test with a \
             justification."
        );
        let push_row = format!("feats.push(\"{key}\")");
        assert!(
            FEATURES_SRC.contains(&push_row),
            "src/cli/features.rs checks `cfg!(feature = \"{key}\")` but never \
             pushes \"{key}\" — footer row for `{key}` is broken (missing \
             `{push_row}`)."
        );
    }
}

/// Reverse direction: every feature string mentioned in
/// src/cli/features.rs (both the `cfg!` guard and the pushed label)
/// must be a declared Cargo feature. Reds on a stale row after a
/// feature is deleted or renamed in Cargo.toml, and on a
/// guard/label typo (pushing a string that isn't a feature).
#[test]
fn every_footer_row_is_a_declared_cargo_feature() {
    let keys = cargo_feature_keys();
    let cfg_feats = quoted_strings_after(FEATURES_SRC, "cfg!(feature = \"");
    assert!(
        !cfg_feats.is_empty(),
        "found no cfg!(feature = \"...\") rows in src/cli/features.rs — grep needle drifted?"
    );
    for feat in &cfg_feats {
        assert!(
            keys.iter().any(|k| k == feat),
            "src/cli/features.rs checks `cfg!(feature = \"{feat}\")` but Cargo.toml \
             declares no such feature — stale or misspelled footer row."
        );
    }
    let pushed = quoted_strings_after(FEATURES_SRC, "feats.push(\"");
    assert!(
        !pushed.is_empty(),
        "found no feats.push(\"...\") rows in src/cli/features.rs — grep needle drifted?"
    );
    for feat in &pushed {
        assert!(
            keys.iter().any(|k| k == feat),
            "src/cli/features.rs pushes footer label \"{feat}\" but Cargo.toml \
             declares no such feature — label/guard mismatch or stale row."
        );
    }
}
