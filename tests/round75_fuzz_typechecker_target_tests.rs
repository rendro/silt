//! Round 75 TEST-1 GAP — typechecker fuzz target lock.
//!
//! Existing fuzz drivers (`fuzz_lexer`, `fuzz_parser`, `fuzz_formatter`,
//! `fuzz_roundtrip`) cover the lexer, parser, and formatter. The
//! typechecker — silt's largest subsystem by line count — had no fuzz
//! target before round 75. These tests pin the new
//! `fuzz/fuzz_targets/fuzz_typechecker.rs` driver and the matching
//! `[[bin]]` entry in `fuzz/Cargo.toml` so a future refactor can't
//! silently delete the coverage.

use std::path::PathBuf;

/// Anchor every check at `CARGO_MANIFEST_DIR` so the lock keeps working
/// from any CWD a harness might choose. Mirrors the pattern in
/// `tests/fuzz_corpus_presence_tests.rs`.
fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn fuzz_typechecker_target_file_exists() {
    let path = manifest_dir().join("fuzz/fuzz_targets/fuzz_typechecker.rs");
    assert!(
        path.exists(),
        "round 75 TEST-1 GAP regressed: \
         fuzz/fuzz_targets/fuzz_typechecker.rs is missing at {}",
        path.display()
    );

    // Sanity-check the body actually exercises the typechecker entry
    // point — a stub file would also "exist" and silently provide no
    // coverage.
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(
        src.contains("fuzz_target!"),
        "fuzz_typechecker.rs is not a libFuzzer target"
    );
    assert!(
        src.contains("typechecker::check"),
        "fuzz_typechecker.rs must invoke `typechecker::check` — \
         that's the entire point of this target"
    );
    assert!(
        src.contains("Lexer::new") && src.contains("parse_program"),
        "fuzz_typechecker.rs must lex + parse before typechecking \
         (typechecker only runs on parsed programs)"
    );
}

#[test]
fn fuzz_cargo_toml_lists_typechecker_target() {
    let path = manifest_dir().join("fuzz/Cargo.toml");
    let toml = std::fs::read_to_string(&path).unwrap();
    assert!(
        toml.contains("name = \"fuzz_typechecker\""),
        "fuzz/Cargo.toml is missing the [[bin]] entry for the new \
         fuzz_typechecker target — cargo fuzz won't find the binary \
         without it. Path checked: {}",
        path.display()
    );
    assert!(
        toml.contains("path = \"fuzz_targets/fuzz_typechecker.rs\""),
        "fuzz/Cargo.toml fuzz_typechecker entry is missing its path \
         field"
    );
}

/// Best-effort: feed the new harness body's logic a handful of seeds
/// from the existing parser corpus to confirm it never panics on
/// inputs the parser already accepts. We can't actually link
/// libfuzzer-sys here (that requires nightly + cargo-fuzz), so we
/// reproduce the same pipeline the harness runs and assert the
/// post-conditions hold.
#[test]
fn fuzz_typechecker_runs_on_existing_corpus() {
    use silt::lexer::Lexer;
    use silt::parser::Parser;
    use silt::typechecker;

    let corpus = manifest_dir().join("fuzz/corpus/fuzz_parser");
    if !corpus.exists() {
        // No corpus — nothing to sanity-check. The other two tests
        // still pin the file's existence and the Cargo entry.
        return;
    }

    let entries: Vec<_> = std::fs::read_dir(&corpus)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().map(|ft| ft.is_file()).unwrap_or(false) && e.file_name() != ".gitkeep"
        })
        .take(10)
        .collect();

    let max_diagnostics = 10_000usize;

    for entry in &entries {
        let bytes = std::fs::read(entry.path()).unwrap();
        let Ok(s) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let Ok(tokens) = Lexer::new(s).tokenize() else {
            continue;
        };
        let Ok(mut program) = Parser::new(tokens).parse_program() else {
            continue;
        };
        let errors = typechecker::check(&mut program);
        assert!(
            errors.len() <= max_diagnostics,
            "typechecker produced {} diagnostics on corpus seed {:?} (cap {})",
            errors.len(),
            entry.path(),
            max_diagnostics
        );
        for (idx, err) in errors.iter().enumerate() {
            assert!(
                !err.message.is_empty(),
                "corpus seed {:?} produced empty-message diagnostic #{}",
                entry.path(),
                idx
            );
            assert!(
                err.span.offset <= s.len(),
                "corpus seed {:?} produced diagnostic #{} with \
                 span.offset {} > source len {}",
                entry.path(),
                idx,
                err.span.offset,
                s.len()
            );
        }
    }
}

/// Lock the corpus directory we expect cargo-fuzz to populate for the
/// new target. `fuzz_corpus_dirs_have_seeds` (in
/// `tests/fuzz_corpus_presence_tests.rs`) iterates every subdir of
/// `fuzz/corpus`, so creating the dir here without a seed would break
/// that lock. Instead we just assert that *if* the dir has been
/// populated, it contains seeds — and otherwise tolerate its absence
/// since cargo-fuzz will create it on first run.
#[test]
fn fuzz_typechecker_corpus_dir_well_formed_if_present() {
    let dir = manifest_dir().join("fuzz/corpus/fuzz_typechecker");
    if !dir.exists() {
        return;
    }
    let seeds: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() != ".gitkeep")
        .collect();
    assert!(
        !seeds.is_empty(),
        "fuzz/corpus/fuzz_typechecker exists but has no seeds — \
         either delete the dir or populate it via fuzz/seed.sh"
    );
}
