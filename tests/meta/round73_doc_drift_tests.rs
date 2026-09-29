//! Round-76 lock: every ```silt fence in `docs/proposals/effect-rows.md`
//! must pass `silt check` (illustrative pseudocode is tagged ```text).
//!
//! The other round-73 doc-drift locks that lived here were doc-spelling
//! checks (deleted) or snippet checks, now golden cases under
//! tests/golden/meta/*/round73_doc_drift_tests__*.

use std::path::{Path, PathBuf};
use std::process::Command;

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round73_{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_main(dir: &Path, src: &str) -> PathBuf {
    let path = dir.join("main.silt");
    std::fs::write(&path, src).unwrap();
    path
}

const EFFECT_DOC_SRC: &str = include_str!("../../docs/proposals/effect-rows.md");

#[test]
fn round76_effect_rows_all_silt_fences_parse() {
    // Positive assertion: every ```silt fence in effect-rows.md must
    // parse via `silt check` (or be tagged ```text/```pseudo). The
    // three problem fences from B4 were retagged to ```text in
    // round 76; this test guards against future drift in either
    // direction (un-retagging, or new ```silt fences with bad
    // syntax).
    let doc = EFFECT_DOC_SRC;

    let mut current: Option<String> = None;
    let mut snippets: Vec<String> = Vec::new();
    for line in doc.lines() {
        let trimmed = line.trim_start();
        if let Some(buf) = current.as_mut() {
            if trimmed.starts_with("```") {
                snippets.push(std::mem::take(buf));
                current = None;
            } else {
                buf.push_str(line);
                buf.push('\n');
            }
        } else if trimmed.starts_with("```silt") {
            current = Some(String::new());
        }
    }

    for (idx, snippet) in snippets.iter().enumerate() {
        let dir = scratch_dir(&format!("round76_effect_silt_{idx}"));
        let main = write_main(&dir, snippet);
        let output = silt_bin()
            .args(["check", main.to_str().unwrap()])
            .output()
            .expect("silt check");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "effect-rows.md ```silt fence #{idx} did NOT parse via `silt check`. \
             Either retag it to ```text/```pseudo (illustrative pseudocode) or \
             rewrite the snippet to runnable silt.\n\
             snippet:\n{snippet}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
