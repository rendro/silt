//! Integration tests for the `silt::manifest` module.
//!
//! Mirrors the temp-dir pattern used in `tests/modules.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use silt::intern;
use silt::manifest::{Dependency, GitRef, Manifest, ManifestError};

// ── Test scaffolding ──────────────────────────────────────────────────

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("silt_manifest_test_{pid}_{nanos}_{n}"));
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

fn write_manifest(dir: &Path, contents: &str) -> PathBuf {
    let path = dir.join("silt.toml");
    fs::write(&path, contents).expect("failed to write silt.toml");
    path
}

fn load_err(contents: &str) -> ManifestError {
    let dir = tempdir();
    let path = write_manifest(&dir, contents);
    Manifest::load(&path).expect_err("expected manifest load to fail")
}

// ── Happy path ────────────────────────────────────────────────────────

#[test]
fn test_minimal_manifest_loads() {
    let dir = tempdir();
    let path = write_manifest(
        &dir,
        r#"
[package]
name = "foo"
version = "0.1.0"
"#,
    );
    let manifest = Manifest::load(&path).expect("load");
    assert_eq!(intern::resolve(manifest.package.name), "foo");
    assert_eq!(manifest.package.version, "0.1.0");
    assert!(manifest.package.edition.is_none());
    assert!(manifest.dependencies.is_empty());
}

#[test]
fn test_manifest_with_path_dep() {
    let dir = tempdir();
    let path = write_manifest(
        &dir,
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { path = "../bar" }
"#,
    );
    let manifest = Manifest::load(&path).expect("load");
    let bar_sym = intern::intern("bar");
    let dep = manifest
        .dependencies
        .get(&bar_sym)
        .expect("expected `bar` dependency");
    match dep {
        Dependency::Path { path } => {
            assert_eq!(path, &PathBuf::from("../bar"));
        }
        other => panic!("expected Path dep, got {other:?}"),
    }
}

#[test]
fn test_manifest_with_edition() {
    let dir = tempdir();
    let path = write_manifest(
        &dir,
        r#"
[package]
name = "foo"
version = "0.1.0"
edition = "2026"
"#,
    );
    let manifest = Manifest::load(&path).expect("load");
    assert_eq!(manifest.package.edition.as_deref(), Some("2026"));
}

// ── Schema errors ─────────────────────────────────────────────────────

#[test]
fn test_missing_name_is_error() {
    let err = load_err(
        r#"
[package]
version = "0.1.0"
"#,
    );
    let msg = err.to_string();
    assert!(
        msg.contains("missing field `name`"),
        "expected missing-name error, got: {msg}"
    );
}

#[test]
fn test_missing_version_is_error() {
    let err = load_err(
        r#"
[package]
name = "foo"
"#,
    );
    let msg = err.to_string();
    assert!(
        msg.contains("missing field `version`"),
        "expected missing-version error, got: {msg}"
    );
}

// ── Identifier validation ─────────────────────────────────────────────

#[test]
fn test_invalid_name_uppercase() {
    let err = load_err(
        r#"
[package]
name = "Foo"
version = "0.1.0"
"#,
    );
    let msg = err.to_string();
    assert!(msg.contains("Foo"), "{msg}");
    assert!(
        msg.to_lowercase().contains("lowercase"),
        "expected message about lowercase, got: {msg}"
    );
}

#[test]
fn test_invalid_name_dot() {
    let err = load_err(
        r#"
[package]
name = "foo.bar"
version = "0.1.0"
"#,
    );
    let msg = err.to_string();
    assert!(msg.contains("foo.bar"), "{msg}");
    assert!(
        msg.to_lowercase().contains("lowercase letters")
            || msg.to_lowercase().contains("identifier"),
        "expected identifier-rule message, got: {msg}"
    );
}

#[test]
fn test_invalid_name_leading_digit() {
    let err = load_err(
        r#"
[package]
name = "1foo"
version = "0.1.0"
"#,
    );
    let msg = err.to_string();
    assert!(
        msg.contains("1foo")
            && (msg.to_lowercase().contains("digit") || msg.to_lowercase().contains("identifier")),
        "expected message about leading digit / identifier, got: {msg}"
    );
}

// ── Version validation ───────────────────────────────────────────────

#[test]
fn test_invalid_version_format() {
    for bad in ["v1", "abc", "1", "1.0", ""] {
        let err = load_err(&format!(
            r#"
[package]
name = "foo"
version = "{bad}"
"#
        ));
        let msg = err.to_string();
        assert!(
            msg.to_lowercase().contains("version"),
            "expected version error for `{bad}`, got: {msg}"
        );
    }
}

// ── Dependency validation ────────────────────────────────────────────

#[test]
fn test_unknown_dep_key() {
    let err = load_err(
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { path = "../bar", branch = "main" }
"#,
    );
    let msg = err.to_string();
    assert!(
        msg.contains("branch") && msg.to_lowercase().contains("unknown"),
        "expected unknown-key error, got: {msg}"
    );
}

// ── Git dependency parsing ────────────────────────────────────────────

#[test]
fn test_git_dep_with_rev_parses() {
    let dir = tempdir();
    let path = write_manifest(
        &dir,
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { git = "https://example.com/bar.git", rev = "abc123def456" }
"#,
    );
    let manifest = Manifest::load(&path).expect("load");
    let bar_sym = intern::intern("bar");
    let dep = manifest
        .dependencies
        .get(&bar_sym)
        .expect("expected bar dependency");
    match dep {
        Dependency::Git { url, ref_spec } => {
            assert_eq!(url, "https://example.com/bar.git");
            match ref_spec {
                GitRef::Rev(s) => assert_eq!(s, "abc123def456"),
                other => panic!("expected GitRef::Rev, got {other:?}"),
            }
        }
        other => panic!("expected Git dep, got {other:?}"),
    }
}

#[test]
fn test_git_dep_with_branch_parses() {
    let dir = tempdir();
    let path = write_manifest(
        &dir,
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { git = "https://example.com/bar.git", branch = "main" }
"#,
    );
    let manifest = Manifest::load(&path).expect("load");
    let bar_sym = intern::intern("bar");
    let dep = manifest
        .dependencies
        .get(&bar_sym)
        .expect("expected bar dependency");
    match dep {
        Dependency::Git { url, ref_spec } => {
            assert_eq!(url, "https://example.com/bar.git");
            match ref_spec {
                GitRef::Branch(s) => assert_eq!(s, "main"),
                other => panic!("expected GitRef::Branch, got {other:?}"),
            }
        }
        other => panic!("expected Git dep, got {other:?}"),
    }
}

#[test]
fn test_git_dep_with_tag_parses() {
    let dir = tempdir();
    let path = write_manifest(
        &dir,
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { git = "https://example.com/bar.git", tag = "v1.2.3" }
"#,
    );
    let manifest = Manifest::load(&path).expect("load");
    let bar_sym = intern::intern("bar");
    let dep = manifest
        .dependencies
        .get(&bar_sym)
        .expect("expected bar dependency");
    match dep {
        Dependency::Git { url, ref_spec } => {
            assert_eq!(url, "https://example.com/bar.git");
            match ref_spec {
                GitRef::Tag(s) => assert_eq!(s, "v1.2.3"),
                other => panic!("expected GitRef::Tag, got {other:?}"),
            }
        }
        other => panic!("expected Git dep, got {other:?}"),
    }
}

#[test]
fn test_git_dep_missing_ref_form_errors() {
    let err = load_err(
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { git = "https://example.com/bar.git" }
"#,
    );
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("rev") && msg.contains("branch") && msg.contains("tag"),
        "expected message naming rev/branch/tag, got: {msg}"
    );
}

#[test]
fn test_git_dep_multiple_ref_forms_errors() {
    let err = load_err(
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { git = "https://example.com/bar.git", rev = "abc123", branch = "main" }
"#,
    );
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("exactly one") || msg.contains("only one"),
        "expected exactly-one wording, got: {msg}"
    );
    assert!(msg.contains("rev"), "expected `rev` in message, got: {msg}");
    assert!(
        msg.contains("branch"),
        "expected `branch` in message, got: {msg}"
    );
}

#[test]
fn test_git_dep_with_path_errors() {
    let err = load_err(
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { git = "https://example.com/bar.git", rev = "abc123", path = "../bar" }
"#,
    );
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("path") && msg.contains("git"),
        "expected message about both path and git, got: {msg}"
    );
}

#[test]
fn test_git_dep_unknown_key_errors() {
    let err = load_err(
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { git = "https://example.com/bar.git", rev = "abc123", branch_pattern = "main-*" }
"#,
    );
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("branch_pattern") && msg.contains("unknown"),
        "expected unknown-key error mentioning branch_pattern, got: {msg}"
    );
}

#[test]
fn test_path_dep_unchanged() {
    // Regression check: existing path-dep behaviour is untouched by the
    // git arm landing in v0.8.
    let dir = tempdir();
    let path = write_manifest(
        &dir,
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { path = "../bar" }
"#,
    );
    let manifest = Manifest::load(&path).expect("load");
    let bar_sym = intern::intern("bar");
    let dep = manifest
        .dependencies
        .get(&bar_sym)
        .expect("expected bar dependency");
    match dep {
        Dependency::Path { path } => assert_eq!(path, &PathBuf::from("../bar")),
        other => panic!("expected Path dep, got {other:?}"),
    }
}

#[test]
fn test_dep_missing_path_key() {
    let err = load_err(
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
bar = { }
"#,
    );
    let msg = err.to_string();
    assert!(
        msg.contains("path"),
        "expected message about missing `path` key, got: {msg}"
    );
}

#[test]
fn test_dep_name_collides_with_builtin() {
    let err = load_err(
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
list = { path = "../list" }
"#,
    );
    let msg = err.to_string();
    assert!(
        msg.contains("list") && msg.to_lowercase().contains("builtin"),
        "expected builtin-collision error, got: {msg}"
    );
}

#[test]
fn test_invalid_dep_name() {
    let err = load_err(
        r#"
[package]
name = "foo"
version = "0.1.0"

[dependencies]
"Foo" = { path = "../foo" }
"#,
    );
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("dependency name") && msg.to_lowercase().contains("lowercase"),
        "expected invalid dep name error, got: {msg}"
    );
}

// ── Parse errors ──────────────────────────────────────────────────────

#[test]
fn test_malformed_toml() {
    // `[[broken` is an unterminated array-of-tables header — the TOML
    // parser today surfaces: "invalid table header\nexpected `.`, `]]`".
    // Lock the message shape so a future regression that swaps in a
    // generic "parse failed" string (or an empty one) is caught.
    let err = load_err("[[broken");
    match err {
        ManifestError::Parse { message, span, .. } => {
            assert!(!message.is_empty(), "expected non-empty parse message");
            let lower = message.to_lowercase();
            // The underlying TOML parser uses phrases like "invalid",
            // "expected", "unexpected", or "parse" in its error text —
            // require at least one of these so a blank/"error"/"oops"
            // regression fails.
            assert!(
                ["invalid", "expected", "unexpected", "parse"]
                    .iter()
                    .any(|kw| lower.contains(kw)),
                "expected TOML-parser-shaped message (invalid/expected/unexpected/parse), \
                 got: {message:?}"
            );
            // `[[broken` is specifically a bad table header; today's
            // toml crate includes the word "table" and/or "header" in
            // its message. Lock on that more specific signal.
            assert!(
                lower.contains("table") || lower.contains("header") || lower.contains("]]"),
                "expected table-header-specific diagnostic, got: {message:?}"
            );
            // The toml 0.8 parser populates a byte-span for the failure
            // site; that's what downstream diagnostic rendering depends
            // on, so make sure we don't regress to None.
            assert!(
                span.is_some(),
                "expected toml parser to supply a byte span, got None"
            );
        }
        other => panic!("expected Parse error, got {other:?}"),
    }
}

// ── Path / discovery ──────────────────────────────────────────────────

#[test]
fn test_manifest_path_recorded() {
    let dir = tempdir();
    let path = write_manifest(
        &dir,
        r#"
[package]
name = "foo"
version = "0.1.0"
"#,
    );
    let manifest = Manifest::load(&path).expect("load");
    assert!(
        manifest.manifest_path.is_absolute(),
        "manifest_path should be absolute, got {:?}",
        manifest.manifest_path
    );
    // The file name component should still be `silt.toml`.
    assert_eq!(
        manifest.manifest_path.file_name().and_then(|s| s.to_str()),
        Some("silt.toml")
    );
}

#[test]
fn test_io_error_when_missing() {
    let dir = tempdir();
    let path = dir.join("silt.toml"); // never written
    let err = Manifest::load(&path).expect_err("expected IO error");
    match err {
        ManifestError::Io(_, p) => {
            assert!(
                p.is_absolute(),
                "expected absolute path in IO error, got {p:?}"
            );
        }
        other => panic!("expected Io error, got {other:?}"),
    }
}

#[test]
fn test_find_walks_up() {
    let root = tempdir();
    let nested = root.join("a").join("b").join("c");
    fs::create_dir_all(&nested).expect("mkdir -p");
    write_manifest(
        &root,
        r#"
[package]
name = "foo"
version = "0.1.0"
"#,
    );
    let found = Manifest::find(&nested).expect("expected to find manifest");
    // `root` may be a symlinked path on macOS (/var vs /private/var), so
    // compare via canonicalize when possible.
    let canon = fs::canonicalize(&found).unwrap_or(found.clone());
    let expected = fs::canonicalize(&root).unwrap_or(root.clone());
    assert_eq!(canon, expected);
}

#[test]
fn test_find_starting_from_file() {
    let root = tempdir();
    let nested = root.join("src");
    fs::create_dir_all(&nested).expect("mkdir -p");
    let main_file = nested.join("main.silt");
    fs::write(&main_file, "fn main() {}").expect("write main");
    write_manifest(
        &root,
        r#"
[package]
name = "foo"
version = "0.1.0"
"#,
    );
    let found = Manifest::find(&main_file).expect("expected to find manifest");
    let canon = fs::canonicalize(&found).unwrap_or(found.clone());
    let expected = fs::canonicalize(&root).unwrap_or(root.clone());
    assert_eq!(canon, expected);
}

#[test]
fn test_find_returns_none() {
    // Use an isolated absolute subtree under temp_dir so we know there is
    // no silt.toml between us and `/`.
    let root = tempdir();
    let nested = root.join("a").join("b");
    fs::create_dir_all(&nested).expect("mkdir -p");
    // Walk from the deepest point; either we find no manifest, or we hit
    // one that lives above the system temp dir (which would be deeply
    // unusual). Treat the latter as a test-environment quirk and skip
    // rather than fail.
    let found = Manifest::find(&nested);
    if let Some(p) = &found {
        let temp_root = std::env::temp_dir();
        assert!(
            !p.starts_with(&temp_root),
            "did not expect to find a silt.toml under our temp tree, got {p:?}"
        );
    }
}

#[test]
fn test_discover_finds_and_loads() {
    let root = tempdir();
    let nested = root.join("src");
    fs::create_dir_all(&nested).expect("mkdir -p");
    write_manifest(
        &root,
        r#"
[package]
name = "demo_pkg"
version = "0.2.1"

[dependencies]
helper = { path = "../helper" }
"#,
    );
    let manifest = Manifest::discover(&nested)
        .expect("discover ok")
        .expect("manifest found");
    assert_eq!(intern::resolve(manifest.package.name), "demo_pkg");
    assert_eq!(manifest.package.version, "0.2.1");
    let helper = intern::intern("helper");
    match manifest.dependencies.get(&helper) {
        Some(Dependency::Path { path }) => assert_eq!(path, &PathBuf::from("../helper")),
        other => panic!("expected helper path dep, got {other:?}"),
    }
}

#[test]
fn test_discover_returns_none_when_no_manifest() {
    let root = tempdir();
    let nested = root.join("a").join("b");
    fs::create_dir_all(&nested).expect("mkdir -p");
    // Same caveat as `test_find_returns_none` — don't fail the suite if
    // an enclosing silt.toml exists outside our control.
    match Manifest::discover(&nested) {
        Ok(None) => {}
        Ok(Some(m)) => {
            let temp_root = std::env::temp_dir();
            assert!(
                !m.manifest_path.starts_with(&temp_root),
                "unexpected manifest discovered inside our temp tree: {:?}",
                m.manifest_path
            );
        }
        Err(e) => panic!("discover failed: {e}"),
    }
}
