//! Round-101 LATENT lock: lexical `normalize_path` (collapse `.`/`..`
//! without touching the filesystem) must have exactly ONE definition,
//! in `silt::lockfile`, with `silt add` (src/cli/add.rs) delegating to
//! it.
//!
//! History: the body was mirrored verbatim between src/lockfile.rs and
//! src/cli/paths.rs, and the paths.rs doc-comment even acknowledged the
//! mirror ("Lockfile resolution does this internally ... we apply the
//! same normalization here") without any delegation or parity lock.
//! Drift scenario: change `ParentDir` handling in lockfile.rs (say, to
//! reject escaping paths) without paths.rs, and `silt add` records a
//! manifest path form that lockfile resolution then normalizes
//! differently — the printed success path and the resolved dep path
//! disagree.
//!
//! Behaviour lock: the single helper collapses `a/./b` and `a/x/../b`,
//! preserves leading `..`, and is a lexical no-op on already-clean paths.

use std::path::{Path, PathBuf};

use silt::lockfile::normalize_path;

#[test]
fn normalize_collapses_curdir_and_parentdir() {
    // `.` components vanish.
    assert_eq!(normalize_path(Path::new("a/./b")), PathBuf::from("a/b"));
    // `..` pops the previous component.
    assert_eq!(normalize_path(Path::new("a/x/../b")), PathBuf::from("a/b"));
    // Mixed.
    assert_eq!(
        normalize_path(Path::new("./a/./x/../b/.")),
        PathBuf::from("a/b")
    );
}

#[test]
fn normalize_preserves_leading_parentdir_segments() {
    // A `..` with nothing to pop is preserved, not dropped — the exact
    // behavior both historical copies shared, and the one `silt add
    // foo --path ../foo` depends on when the manifest stores a
    // relative-with-`..` form.
    assert_eq!(normalize_path(Path::new("../a")), PathBuf::from("../a"));
    assert_eq!(
        normalize_path(Path::new("a/../../b")),
        PathBuf::from("../b")
    );
}

#[test]
fn normalize_is_identity_on_clean_paths() {
    for clean in ["a/b/c", "a", "deps/util"] {
        assert_eq!(
            normalize_path(Path::new(clean)),
            PathBuf::from(clean),
            "lexical normalization must be a no-op on already-clean \
             relative paths"
        );
    }
}
