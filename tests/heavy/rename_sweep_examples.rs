//! The rename sweep (see `rename_sweep`) over the examples: a fresh
//! name at every identifier, a sample of the distinct renames applied
//! and compared by default and every one with `SILT_RENAME_FULL=1`.

use crate::rename_sweep::{repo, silt_files, sweep};

/// The examples, each as a case of its own directory: a fresh name at
/// every identifier; every eighth distinct rename is applied and
/// compared, every one with `SILT_RENAME_FULL=1`.
#[test]
fn every_example_renames_by_meaning() {
    let every = match std::env::var_os("SILT_RENAME_FULL") {
        Some(v) if v != "0" => 1,
        _ => 8,
    };
    let dir = repo().join("examples");
    let mut broken = Vec::new();
    let (mut asked, mut renamed, mut refused) = (0, 0, 0);
    for file in silt_files(&dir) {
        // A file in a subdirectory is a module of a package example.
        if file.components().count() > 1 {
            continue;
        }
        let entry = file.to_string_lossy().into_owned();
        let outcome = sweep(&dir, &entry, &entry, false, false, every);
        asked += outcome.asked;
        renamed += outcome.renamed;
        refused += outcome.refused;
        broken.extend(outcome.broken);
    }
    eprintln!("examples: {asked} asked, {renamed} renamed, {refused} refused");
    assert!(asked > 1000, "only {asked} identifiers");
    assert!(
        broken.is_empty(),
        "{} broken rename(s) in the examples:\n{}",
        broken.len(),
        broken.join("\n")
    );
}
