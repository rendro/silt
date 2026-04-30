//! Lock test for the audit-round-65 LATENT fix: typo'd top-level keys
//! and table headers in `silt.toml` (e.g. `descrption` under `[package]`,
//! or a misspelled `[depndencies]` section) must now be rejected at
//! parse time rather than silently accepted as if absent.
//!
//! The fix adds `#[serde(deny_unknown_fields)]` to `RawManifest`,
//! `RawPackage`, and `RawLints` in `src/manifest.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use silt::manifest::{Manifest, ManifestError};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "silt_manifest_unknown_fields_test_{pid}_{nanos}_{n}"
    ));
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

fn write_manifest(dir: &Path, contents: &str) -> PathBuf {
    let path = dir.join("silt.toml");
    fs::write(&path, contents).expect("failed to write silt.toml");
    path
}

fn parse_manifest_str(contents: &str) -> Result<Manifest, ManifestError> {
    let dir = tempdir();
    let path = write_manifest(&dir, contents);
    Manifest::load(&path)
}

#[test]
fn manifest_rejects_unknown_top_level_key() {
    let bad = r#"
        [package]
        name = "x"
        version = "0.1.0"
        descrption = "typo for description"
    "#;
    let result = parse_manifest_str(bad);
    assert!(
        result.is_err(),
        "manifest should reject unknown [package] key `descrption`"
    );
    let msg = format!("{:?}", result.unwrap_err());
    assert!(
        msg.contains("descrption") || msg.contains("unknown field"),
        "error message should mention the unknown key, got: {msg}"
    );
}

#[test]
fn manifest_rejects_unknown_section() {
    let bad = r#"
        [package]
        name = "x"
        version = "0.1.0"

        [depndencies]
        foo = "1.0"
    "#;
    let result = parse_manifest_str(bad);
    assert!(
        result.is_err(),
        "manifest should reject unknown section `depndencies`"
    );
}

#[test]
fn manifest_accepts_well_known_fields() {
    // `description` is not yet a recognized [package] field in silt; once
    // (and if) it lands, this test should be updated. For now we use the
    // currently-recognized keys — `name`, `version`, `edition` — to
    // confirm that the deny_unknown_fields gate doesn't break valid
    // manifests.
    let good = r#"
        [package]
        name = "x"
        version = "0.1.0"
        edition = "2026"
    "#;
    let result = parse_manifest_str(good);
    assert!(
        result.is_ok(),
        "manifest with no unknown keys must still parse, got: {:?}",
        result.err()
    );
}

#[test]
fn manifest_rejects_unknown_lints_key() {
    // `[lints]` is also gated; a typo like `strict-efects` must not be
    // silently accepted as the default.
    let bad = r#"
        [package]
        name = "x"
        version = "0.1.0"

        [lints]
        strict-efects = true
    "#;
    let result = parse_manifest_str(bad);
    assert!(
        result.is_err(),
        "manifest should reject unknown [lints] key `strict-efects`"
    );
}
