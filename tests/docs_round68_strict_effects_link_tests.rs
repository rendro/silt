//! Round-68 DOC agent locks for the `--strict-effects` discoverability gap.
//!
//! `--strict-effects` is a real CLI flag on `silt run`, `silt check`, and
//! `silt test`, and `docs/strict-effects-migration.md` is a complete
//! migration guide. But before this round the migration guide was orphaned:
//! nothing under the README path linked to it, so users had no way to find
//! it. These tests pin the three new one-line links plus the flag name
//! itself against accidental rename or deletion.

use std::process::Command;

// ────────────────────────────────────────────────────────────────────
// Doc grep-presence locks
// ────────────────────────────────────────────────────────────────────

#[test]
fn readme_links_to_strict_effects_migration_guide() {
    let doc = include_str!("../README.md");
    assert!(
        doc.contains("strict-effects-migration"),
        "README.md must link to docs/strict-effects-migration.md so \
         users discovering silt from the repo root can find the \
         migration guide for the `--strict-effects` flag"
    );
}

#[test]
fn getting_started_links_to_strict_effects_migration_guide() {
    let doc = include_str!("../docs/getting-started.md");
    assert!(
        doc.contains("strict-effects-migration"),
        "docs/getting-started.md must link to strict-effects-migration.md \
         so users following the onboarding path can find the migration \
         guide for the `--strict-effects` flag"
    );
}

#[test]
fn language_guide_links_to_strict_effects_migration_guide() {
    let doc = include_str!("../docs/language-guide.md");
    assert!(
        doc.contains("strict-effects-migration"),
        "docs/language-guide.md must link to strict-effects-migration.md \
         from its `Other Guides` section so readers exploring the \
         language docs can find the migration guide"
    );
}

// ────────────────────────────────────────────────────────────────────
// Flag-name lock: `silt run --help` mentions `--strict-effects`
// ────────────────────────────────────────────────────────────────────

#[test]
fn silt_run_help_mentions_strict_effects_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["run", "--help"])
        .output()
        .expect("silt run --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "`silt run --help` should succeed; got status {:?}",
        output.status
    );
    assert!(
        stdout.contains("--strict-effects"),
        "`silt run --help` must list `--strict-effects` exactly so \
         the flag name in the migration guide and the README links \
         stays in sync with the CLI; got stdout:\n{stdout}"
    );
}
