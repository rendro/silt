//! Tests for `silt::git`. The hermetic tests cover cache path
//! computation and ref-shape validation; the network-gated tests (only
//! enabled when `SILT_GIT_INTEGRATION_TESTS=1`) exercise `ls-remote` and
//! a real clone against silt's own GitHub repo.

use std::path::Path;

use silt::git::{self, GitRef};

// ── Hermetic tests (always run) ───────────────────────────────────────

#[test]
fn test_cache_dir_is_under_xdg_or_home() {
    let dir = git::cache_dir().expect("cache_dir succeeds");
    let s = dir.to_string_lossy();
    // Path must contain `silt` and `git` segments regardless of OS.
    assert!(
        s.contains("silt") && s.contains("git"),
        "cache_dir should contain silt and git segments, got {s}"
    );
    assert!(
        Path::new(&*s).is_absolute(),
        "cache_dir should be absolute, got {s}"
    );
}

#[test]
fn test_cache_for_includes_url_hash_and_sha() {
    let dir =
        git::cache_for("https://example.com/foo", "abc123def4567890").expect("cache_for succeeds");
    let s = dir.to_string_lossy();
    // The resolved SHA appears verbatim as the leaf component.
    assert!(
        s.contains("abc123def4567890"),
        "expected resolved SHA in path, got {s}"
    );
    // The URL hash isn't the URL itself, but the result is deterministic
    // (next test) — here we just verify the path actually has more
    // structure than just the cache root.
    let cache_root = git::cache_dir().expect("cache_dir succeeds");
    assert!(
        dir.starts_with(&cache_root),
        "expected {} to start with cache root {}",
        dir.display(),
        cache_root.display()
    );
    // Two subdirectory levels under the root: <url-hash>/<sha>.
    let rel = dir
        .strip_prefix(&cache_root)
        .expect("dir under cache root");
    assert_eq!(
        rel.components().count(),
        2,
        "expected two path components under cache root, got {rel:?}"
    );
}

#[test]
fn test_cache_for_consistent_for_same_url_and_sha() {
    let a = git::cache_for("https://example.com/foo", "deadbeef0000000").unwrap();
    let b = git::cache_for("https://example.com/foo", "deadbeef0000000").unwrap();
    assert_eq!(a, b);
    let c = git::cache_for("https://example.com/bar", "deadbeef0000000").unwrap();
    assert_ne!(a, c, "different URLs must produce different cache paths");
    let d = git::cache_for("https://example.com/foo", "facefeed0000000").unwrap();
    assert_ne!(a, d, "different SHAs must produce different cache paths");
}

#[test]
fn test_resolve_ref_rev_validates_format() {
    // `notahex` is not a valid SHA shape — should fail without ever
    // contacting the network.
    let bad = git::resolve_ref("https://example.invalid/repo", &GitRef::Rev("notahex".into()));
    assert!(bad.is_err(), "expected error for non-hex rev");

    // 7-hex SHA is the minimum acceptable shape; this should succeed
    // (returning the lowercased SHA verbatim) without any network call
    // because Rev resolution is offline.
    let ok = git::resolve_ref(
        "https://example.invalid/repo",
        &GitRef::Rev("AbC1234".into()),
    )
    .expect("7-hex SHA should resolve offline");
    assert_eq!(ok, "abc1234", "expected lowercased SHA");
}

// ── Network-gated tests ───────────────────────────────────────────────

fn skip_unless_network() -> bool {
    std::env::var("SILT_GIT_INTEGRATION_TESTS").is_err()
}

const SILT_REPO: &str = "https://github.com/rendro/silt";

#[test]
fn test_resolve_ref_branch_against_real_repo() {
    if skip_unless_network() {
        return;
    }
    let sha = git::resolve_ref(SILT_REPO, &GitRef::Branch("main".into()))
        .expect("resolve main branch of silt repo");
    assert_eq!(sha.len(), 40, "expected full SHA, got `{sha}`");
    assert!(
        sha.chars().all(|c| c.is_ascii_hexdigit()),
        "expected hex SHA, got `{sha}`"
    );
}

#[test]
fn test_verify_reachable_succeeds_for_real_repo() {
    if skip_unless_network() {
        return;
    }
    git::verify_reachable(SILT_REPO).expect("silt repo is reachable");
}

#[test]
fn test_verify_reachable_fails_for_bogus_url() {
    if skip_unless_network() {
        return;
    }
    let result = git::verify_reachable("https://example.invalid/definitely-not-a-repo.git");
    assert!(result.is_err(), "expected bogus URL to fail reachability");
}

#[test]
fn test_fetch_to_cache_actually_clones() {
    if skip_unless_network() {
        return;
    }
    // Resolve `main` first to get a known-good SHA, then fetch it.
    let sha = git::resolve_ref(SILT_REPO, &GitRef::Branch("main".into()))
        .expect("resolve main branch");
    let dir = git::fetch_to_cache(SILT_REPO, &sha).expect("clone to cache");
    assert!(
        dir.join("silt.toml").is_file(),
        "expected cache dir to contain silt.toml, got {}",
        dir.display()
    );
    // Idempotency: a second call returns the same dir without re-cloning.
    let dir2 = git::fetch_to_cache(SILT_REPO, &sha).expect("idempotent fetch");
    assert_eq!(dir, dir2);
}
