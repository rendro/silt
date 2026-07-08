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
//! Locks, in order:
//!   1. Source-grep: the `Component::ParentDir` pop-or-push fingerprint
//!      is gone from src/cli/paths.rs (this assertion FAILS on the
//!      pre-fix tree) and present in src/lockfile.rs.
//!   2. Tree-wide uniqueness: walking src/**/*.rs, exactly one file
//!      contains the fingerprint — so the duplicate can't quietly come
//!      back somewhere else either.
//!   3. Delegation: src/cli/add.rs imports `normalize_path` from
//!      `silt::lockfile`, not from a local copy.
//!   4. Behavior: the surviving helper still collapses `a/./b` and
//!      `a/x/../b`, preserves leading `..`, and is a lexical no-op on
//!      already-clean paths — proving the dedup changed nothing.

use std::path::{Path, PathBuf};

use silt::lockfile::normalize_path;

const PATHS_SRC: &str = include_str!("../src/cli/paths.rs");
const LOCKFILE_SRC: &str = include_str!("../src/lockfile.rs");
const ADD_SRC: &str = include_str!("../src/cli/add.rs");

/// The unique fingerprint of the lexical-normalize loop body.
const FINGERPRINT: &str = "Component::ParentDir";

// ── 1. the CLI-side mirror is gone; the lockfile definition remains ──

#[test]
fn cli_paths_no_longer_carries_the_normalize_body() {
    assert!(
        !PATHS_SRC.contains(FINGERPRINT),
        "src/cli/paths.rs contains the `Component::ParentDir` \
         pop-or-push loop again. That is the body of the lexical \
         normalize_path duplicated from src/lockfile.rs (round-101 \
         LATENT). Delete it and delegate to \
         `silt::lockfile::normalize_path` instead — otherwise the two \
         copies can drift and `silt add` will record a manifest path \
         that lockfile resolution normalizes differently."
    );
}

#[test]
fn lockfile_carries_the_single_normalize_definition() {
    assert!(
        LOCKFILE_SRC.contains("pub fn normalize_path"),
        "src/lockfile.rs must define `pub fn normalize_path` — it is \
         the single lexical normalizer, and must stay `pub` (not \
         pub(crate)) because the CLI lives in the bin crate and \
         delegates across the lib boundary."
    );
    assert!(
        LOCKFILE_SRC.contains(FINGERPRINT),
        "src/lockfile.rs lost the `Component::ParentDir` pop-or-push \
         loop. If normalize_path moved to a new home, re-aim this lock \
         (and the delegation lock below) at the new single definition."
    );
}

// ── 2. tree-wide: exactly one file under src/ contains the body ──────

#[test]
fn normalize_body_appears_in_exactly_one_source_file() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits: Vec<PathBuf> = Vec::new();
    collect_fingerprint_hits(&src_root, &mut hits);
    assert_eq!(
        hits,
        vec![src_root.join("lockfile.rs")],
        "the `Component::ParentDir` lexical-normalize fingerprint must \
         appear in exactly one file under src/ (src/lockfile.rs). \
         Extra hits mean the normalize_path body was duplicated again \
         (round-101 LATENT); a different single hit means it moved — \
         update the delegation call sites and this lock together."
    );
}

fn collect_fingerprint_hits(dir: &Path, hits: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_fingerprint_hits(&path, hits);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            if src.contains(FINGERPRINT) {
                hits.push(path);
            }
        }
    }
}

// ── 3. `silt add` delegates to the lockfile definition ───────────────

#[test]
fn silt_add_delegates_to_lockfile_normalize_path() {
    assert!(
        ADD_SRC.contains("use silt::lockfile::{Lockfile, normalize_path};")
            || ADD_SRC.contains("silt::lockfile::normalize_path"),
        "src/cli/add.rs must obtain normalize_path from \
         silt::lockfile — the one lexical normalizer — so the path \
         form `silt add` records in silt.toml is normalized by exactly \
         the rules lockfile resolution applies when resolving it."
    );
    assert!(
        ADD_SRC.contains("normalize_path(&absolute_dep_path)"),
        "src/cli/add.rs no longer normalizes the resolved dep path \
         before recording it. If the call site was renamed or moved, \
         re-aim this lock; if it was deleted, `silt add foo --path \
         ../x/./y` records an un-collapsed path in silt.toml."
    );
}

// ── 4. behavior of the surviving helper is unchanged ─────────────────

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
