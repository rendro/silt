//! Regression lock: each fuzz target's corpus dir must ship at least one
//! seed file (other than .gitkeep) so libFuzzer doesn't cold-start the
//! campaign. See fuzz/seed.sh for the canonical populator.

#[test]
fn fuzz_corpus_dirs_have_seeds() {
    use std::fs;
    // Iterate fuzz/corpus/<target>/ subdirs; each must have >= 1 file other than .gitkeep.
    let base = std::path::Path::new("fuzz/corpus");
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

/// Textual lock: the parser fuzz target must exercise both `parse_program`
/// and `parse_program_recovering` so LSP's recovery entry point gets
/// libFuzzer corpus-driven exploration.
#[test]
fn fuzz_parser_target_invokes_both_entry_points() {
    let path = std::path::Path::new("fuzz/fuzz_targets/fuzz_parser.rs");
    assert!(
        path.exists(),
        "fuzz_parser.rs missing at {}",
        path.display()
    );
    let src = std::fs::read_to_string(path).unwrap();
    assert!(
        src.contains("fuzz_target!"),
        "fuzz_parser.rs is not a libFuzzer target"
    );
    assert!(
        src.contains("parse_program("),
        "fuzz_parser.rs must invoke parse_program()"
    );
    assert!(
        src.contains("parse_program_recovering"),
        "fuzz_parser.rs must invoke parse_program_recovering() — LSP's primary recovery path"
    );
}
