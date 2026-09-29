//! Regression lock for the `math.acos(1.0)` doc snippet output that
//! lives in the `math.acos` doc string registered by
//! `src/typechecker/builtins/math.rs` (round 62 phase-2 LSP doc
//! inlining moved this prose out of the deleted `docs/stdlib/math.md`).
//!
//! Round 60 audit L11: the doc snippet's comment claimed the program
//! prints `0.0`, but silt's Float Display drops the trailing `.0` for
//! integer-valued floats, so the program actually prints `0`. The fix
//! updates the doc to match the CLI output and leaves a parenthetical
//! explaining the Display-formatting quirk.
//!
//! This test does both: (a) introspects the registered builtin doc
//! string for `math.acos` and locks the corrected `-- 0` annotation,
//! and (b) actually runs the snippet through the `silt` CLI and
//! asserts the stdout is a bare `0` so the doc can't drift back.

use std::process::Command;

fn read_math_acos_doc() -> String {
    let docs = silt::typechecker::builtin_docs();
    docs.get("math.acos")
        .cloned()
        .expect("math.acos doc must be registered (see src/typechecker/builtins/math.rs)")
}

/// The comment in the `math.acos` snippet must show the actual CLI
/// output (`0`), not the mathematically-decorated `0.0` the earlier
/// doc claimed. The parenthetical about Display-formatting is a free
/// explanation for the reader; we don't pin its exact wording, only
/// that the `println(angle)  -- 0` form is present.
#[test]
fn math_acos_doc_snippet_shows_bare_zero_output() {
    let doc = read_math_acos_doc();
    // The `  -- 0.0` claim must not reappear as the annotated output.
    assert!(
        !doc.contains("println(angle)  -- 0.0"),
        "math.acos builtin doc still claims `math.acos(1.0) else 0.0` \
         prints `0.0`. Silt's Float Display drops the trailing `.0` \
         for integer-valued floats, so the actual stdout is `0`."
    );
    // The corrected annotation must be present.
    assert!(
        doc.contains("println(angle)  -- 0"),
        "math.acos builtin doc must annotate the stdout as `0` (no \
         trailing `.0`), matching the Float Display behaviour. Got \
         doc:\n{doc}"
    );
}

/// Run the snippet end-to-end: compile and run via the `silt` CLI,
/// assert the stdout is exactly a bare `0` line. This is the live
/// lock — if the Float Display ever grows a trailing `.0` again, the
/// doc annotation and this assertion fail together.
#[test]
fn math_acos_doc_snippet_runs_and_prints_bare_zero() {
    let src = r#"
import math
fn main() {
    let angle = math.acos(1.0) else 0.0
    println(angle)
}
"#;
    let tmp = std::env::temp_dir().join("silt_docs_math_acos_snippet.silt");
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "math.acos doc snippet must run cleanly; stdout={stdout:?} stderr={stderr:?}"
    );
    // We strip one trailing newline (println's) and assert the line is
    // exactly `0` — NOT `0.0`. If Float Display ever changes behaviour,
    // this test breaks along with the doc's annotation.
    let printed = stdout.trim_end_matches('\n');
    assert_eq!(
        printed, "0",
        "math.acos(1.0) else 0.0 must print a bare `0` (no trailing \
         `.0`); got {printed:?} (full stdout={stdout:?})"
    );
}
