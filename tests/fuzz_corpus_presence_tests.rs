//! Regression lock: each fuzz target's corpus dir must ship at least one
//! seed file (other than .gitkeep) so libFuzzer doesn't cold-start the
//! campaign. See fuzz/seed.sh for the canonical populator.

#[test]
fn fuzz_corpus_dirs_have_seeds() {
    use std::fs;
    // Iterate fuzz/corpus/<target>/ subdirs; each must have >= 1 file other than .gitkeep.
    let base = std::path::Path::new("fuzz/corpus");
    if !base.exists() {
        return;
    } // Skip if fuzz not configured locally.
    let mut empty_dirs = Vec::new();
    for entry in fs::read_dir(base).unwrap() {
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
