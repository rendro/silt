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
//! Strategy: parse the `[features]` table keys out of `include_str!`'d
//! Cargo.toml (line scanner; `toml` is not a dev-dependency), subtract
//! the documented-internal set, and compare with the footer that the
//! built `silt --help` actually prints. Each user-facing key must be
//! listed exactly when this build enables it; a key this test does not
//! know yet fails until it is added to `feature_enabled` (and to the
//! footer).

const CARGO_TOML: &str = include_str!("../../Cargo.toml");

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

/// Is `key` compiled into this build? `cfg!` needs a literal, so every
/// user-facing Cargo feature is spelled out here.
fn feature_enabled(key: &str) -> bool {
    match key {
        "repl" => cfg!(feature = "repl"),
        "lsp" => cfg!(feature = "lsp"),
        "watch" => cfg!(feature = "watch"),
        "local-clock" => cfg!(feature = "local-clock"),
        "http" => cfg!(feature = "http"),
        "tcp" => cfg!(feature = "tcp"),
        "tcp-tls" => cfg!(feature = "tcp-tls"),
        "postgres" => cfg!(feature = "postgres"),
        "postgres-tls" => cfg!(feature = "postgres-tls"),
        other => panic!(
            "Cargo.toml declares feature `{other}` that this test does not know. \
             Add a row for it to enabled_features() in src/cli/features.rs and to \
             feature_enabled() here, or (for a genuinely internal feature) add it \
             to INTERNAL_FEATURES with a justification."
        ),
    }
}

/// The comma-separated entries of the `Enabled features:` line printed
/// by `silt --help`.
fn footer_features() -> Vec<String> {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("--help")
        .output()
        .expect("spawn silt --help");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("Enabled features:"))
        .unwrap_or_else(|| panic!("no `Enabled features:` line in silt --help:\n{stdout}"))
        .trim();
    if line == "(none)" {
        return Vec::new();
    }
    line.split(',').map(|f| f.trim().to_string()).collect()
}

/// Both directions: every user-facing Cargo feature this build enables
/// is in the footer, and every footer entry is an enabled, declared,
/// user-facing Cargo feature.
#[test]
fn help_footer_lists_exactly_the_enabled_cargo_features() {
    let keys = cargo_feature_keys();
    let mut expected: Vec<String> = keys
        .iter()
        .filter(|k| !INTERNAL_FEATURES.contains(&k.as_str()))
        .filter(|k| feature_enabled(k))
        .cloned()
        .collect();
    let mut footer = footer_features();
    expected.sort();
    footer.sort();
    assert_eq!(
        footer, expected,
        "`silt --help` footer disagrees with the Cargo features enabled in \
         this build (left: footer, right: enabled user-facing features)"
    );
}
