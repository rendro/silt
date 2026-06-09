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

/// Lock the corpus directory for the typechecker target. Round 92
/// committed seeds for it (mirroring the other four targets), so its
/// absence — or an empty dir — is a regression, not a fresh-clone
/// artifact.
#[test]
fn fuzz_typechecker_corpus_dir_has_seeds() {
    let dir = manifest_dir().join("fuzz/corpus/fuzz_typechecker");
    assert!(
        dir.exists(),
        "fuzz/corpus/fuzz_typechecker is missing — round 92 committed \
         seeds for the typechecker target; re-run fuzz/seed.sh to \
         repopulate"
    );
    let seeds: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() != ".gitkeep")
        .collect();
    assert!(
        !seeds.is_empty(),
        "fuzz/corpus/fuzz_typechecker exists but has no seeds — \
         populate it via fuzz/seed.sh"
    );
}

// ── Round 92: harness-wiring parity lock ────────────────────────────
//
// Round 75 added the fuzz_typechecker target but never wired it into
// any harness: seed.sh, local.sh, the fuzz-nightly.yml matrix, and the
// ci.yml corpus-replay job all hardcoded the original four targets, so
// the new target was never compiled or run anywhere — even a compile
// error in it would have shipped silently.
//
// Rather than grep for `fuzz_typechecker` by name (which would rot the
// moment a sixth target lands), derive the authoritative target list
// from fuzz/Cargo.toml's `[[bin]]` entries and assert every registered
// target appears in every harness surface. Same self-maintaining
// philosophy as tests/comprehensive_module_function_parity_tests.rs:
// adding a target without wiring it — or de-wiring an existing one —
// fails this test with a message naming the missing surface.

/// Parse the `[[bin]]` target names out of `fuzz/Cargo.toml`.
fn registered_fuzz_targets() -> Vec<String> {
    let toml = std::fs::read_to_string(manifest_dir().join("fuzz/Cargo.toml")).unwrap();
    let mut targets = Vec::new();
    let mut in_bin = false;
    for line in toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_bin = line == "[[bin]]";
            continue;
        }
        if in_bin {
            if let Some(rest) = line.strip_prefix("name = \"") {
                if let Some(name) = rest.strip_suffix('"') {
                    targets.push(name.to_string());
                }
            }
        }
    }
    assert!(
        targets.len() >= 5,
        "expected at least the 5 known fuzz targets in fuzz/Cargo.toml \
         [[bin]] entries, parsed only {targets:?} — did the manifest \
         format change?"
    );
    targets
}

#[test]
fn every_registered_fuzz_target_is_wired_into_every_harness() {
    // Each surface a target must appear in for it to actually run.
    // (path-relative-to-repo-root, human description)
    let surfaces = [
        (
            "fuzz/seed.sh",
            "corpus seeding loop — target starts every campaign cold",
        ),
        (
            "fuzz/local.sh",
            "`all` loop / usage text — local discovery skips the target",
        ),
        (
            ".github/workflows/fuzz-nightly.yml",
            "nightly matrix — no scheduled discovery fuzzing ever runs",
        ),
        (
            ".github/workflows/ci.yml",
            "fuzz-corpus replay job — target is never even compiled in CI",
        ),
    ];

    let mut missing = Vec::new();
    for (path, why) in &surfaces {
        let full = manifest_dir().join(path);
        let content = std::fs::read_to_string(&full)
            .unwrap_or_else(|e| panic!("cannot read harness surface {path}: {e}"));
        for target in registered_fuzz_targets() {
            if !content.contains(&target) {
                missing.push(format!("{target} absent from {path} ({why})"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "fuzz target(s) registered in fuzz/Cargo.toml but not wired \
         into all harness surfaces — they will silently never run:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn every_registered_fuzz_target_has_a_source_file_and_corpus_dir() {
    for target in registered_fuzz_targets() {
        let src = manifest_dir().join(format!("fuzz/fuzz_targets/{target}.rs"));
        assert!(
            src.exists(),
            "fuzz/Cargo.toml registers {target} but {} does not exist",
            src.display()
        );
        let corpus = manifest_dir().join(format!("fuzz/corpus/{target}"));
        assert!(
            corpus.exists(),
            "no committed corpus dir for {target} at {} — the ci.yml \
             replay gate would have nothing to replay; seed it via \
             fuzz/seed.sh and commit the seeds like the other targets",
            corpus.display()
        );
    }
}
