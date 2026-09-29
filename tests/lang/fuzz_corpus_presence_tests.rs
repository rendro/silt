//! Regression lock: each fuzz target's corpus dir must ship at least one
//! seed file (other than .gitkeep) so libFuzzer doesn't cold-start the
//! campaign. See fuzz/seed.sh for the canonical populator.

/// Returns the absolute path to the fuzz corpus root, anchored at
/// `CARGO_MANIFEST_DIR` so the lock is robust against any harness that
/// chdir's before running tests (a relative path would silently skip
/// the check from the wrong CWD).
fn corpus_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus")
}

#[test]
fn fuzz_corpus_dirs_have_seeds() {
    use std::fs;
    // Iterate fuzz/corpus/<target>/ subdirs; each must have >= 1 file other than .gitkeep.
    let base = corpus_root();
    if std::env::var("SILT_CI_NO_FUZZ").is_ok() {
        return;
    }
    assert!(
        base.exists(),
        "fuzz/corpus missing at {} — corpus is committed to the repo; \
         set SILT_CI_NO_FUZZ=1 to skip this lock in environments that \
         legitimately strip corpus.",
        base.display()
    );
    let mut empty_dirs = Vec::new();
    for entry in fs::read_dir(&base).unwrap() {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_dir() {
            continue;
        }
        let contents: Vec<_> = fs::read_dir(entry.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name() != ".gitkeep")
            .collect();
        if contents.is_empty() {
            empty_dirs.push(entry.path());
        }
    }
    assert!(
        empty_dirs.is_empty(),
        "fuzz corpus dirs without seeds: {:?}",
        empty_dirs
    );
}

/// Regression lock: the corpus path must be absolute (anchored at the
/// crate manifest dir) so a harness that chdir's before `cargo test`
/// doesn't silently make `fuzz_corpus_dirs_have_seeds` a no-op.
#[test]
fn fuzz_corpus_root_is_absolute() {
    let base = corpus_root();
    assert!(
        base.is_absolute(),
        "fuzz corpus root must be absolute (anchored via CARGO_MANIFEST_DIR), got {:?}",
        base
    );
}
