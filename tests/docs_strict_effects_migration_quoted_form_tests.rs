//! Round-64 BROKEN doc-finding lock.
//!
//! `docs/strict-effects-migration.md` previously quoted the effect set
//! inside the `error[type]:` example like `effect '!{fs, io}'`. The
//! actual diagnostic at `src/typechecker/inference.rs:845` emits
//! `effect !{fs, io}` (no surrounding `'…'` quotes). The mismatch
//! defeats copy-paste search of the diagnostic text.
//!
//! These two locks double-gate the regression:
//!
//! 1. SOURCE-LEVEL grep: the migration doc string never contains
//!    `effect '!{` (the broken form). This is the audit-mandated
//!    weak-gate replacement — a "construct and format" test would not
//!    catch a doc revert.
//! 2. RUNTIME end-to-end: a real `silt check --strict-effects` run
//!    on a small fixture emits stderr that does NOT contain
//!    `effect '!{` and DOES contain `effect !{`. Catches the
//!    diagnostic itself drifting back to the quoted form.

use std::process::Command;

const DOC: &str = include_str!("../docs/strict-effects-migration.md");

#[test]
fn migration_doc_does_not_quote_effect_set_inline() {
    assert!(
        !DOC.contains("effect '!{"),
        "docs/strict-effects-migration.md must not quote the effect set in error examples; \
         the actual diagnostic emits `effect !{{...}}` without surrounding quotes \
         (see src/typechecker/inference.rs:845)"
    );
}

#[test]
fn migration_doc_uses_unquoted_effect_set_form() {
    // Positive shape: the doc still contains the unquoted form
    // somewhere (otherwise the grep above would silently pass after a
    // wholesale deletion of the example).
    assert!(
        DOC.contains("effect !{fs, io}"),
        "docs/strict-effects-migration.md should still contain at least one \
         `effect !{{fs, io}}` example to mirror the actual diagnostic format"
    );
}

#[test]
fn strict_effects_diagnostic_emits_unquoted_effect_set() {
    // End-to-end runtime lock: the diagnostic itself uses the
    // unquoted form. This is the second gate — catches the source
    // formatter drifting even if the doc stays in sync.
    let dir = std::env::temp_dir().join("silt_docs_strict_effects_quoted_form");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let main = dir.join("main.silt");
    std::fs::write(
        &main,
        "import io\n\
         fn load_settings(path: String) -> Result(String, IoError) =\n\
         \x20\x20io.read_file(path)\n\
         fn main() {\n\
         \x20\x20let _settings = load_settings(\"config.toml\")\n\
         \x20\x20()\n\
         }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["check", "--strict-effects", main.to_str().unwrap()])
        .output()
        .expect("failed to run silt check");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains("effect '!{"),
        "strict-effects diagnostic must not surround the effect set with `'…'`; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("effect !{"),
        "strict-effects diagnostic must contain unquoted `effect !{{...}}` form; got stderr:\n{stderr}"
    );
}
