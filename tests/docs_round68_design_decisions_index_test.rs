//! Round-68 DOC agent lock for the design-decisions.md "Newline
//! Sensitivity" finding.
//!
//! `docs/language/design-decisions.md` previously listed "index" as a
//! postfix operator (alongside function call, `?`, and trailing
//! closure). That was misleading: `xs[i]` is reserved syntax that the
//! parser rejects (see `src/parser.rs::parse_index_expr`), so it is
//! not a real postfix operator at all.
//!
//! Two-part lock:
//!
//! 1. Source-grep lock: the misleading enumeration ("function call,
//!    `?`, trailing closure, index") must no longer appear in the
//!    doc. The corrected enumeration without "index" must be present,
//!    along with the explanatory note about reserved bracket
//!    indexing.
//! 2. Behavioral lock: the parser still rejects `xs[0]` with the
//!    documented diagnostic. This pins the doc claim ("not a real
//!    postfix operator") against a future world where someone DOES
//!    implement indexing without updating the docs.
//!
//! See `tests/docs_round67_walker_tests.rs` for the subprocess pattern
//! this file mirrors.

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn read_doc(rel: &str) -> String {
    let path = manifest_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round68_{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_main(dir: &Path, src: &str) -> PathBuf {
    let path = dir.join("main.silt");
    std::fs::write(&path, src).unwrap();
    path
}

// ────────────────────────────────────────────────────────────────────
// Source-grep lock: the "index" entry must be gone from the
// Newline-Sensitivity postfix-operator enumeration.
// ────────────────────────────────────────────────────────────────────

const DOC_PATH: &str = "docs/language/design-decisions.md";
const DOC_SRC: &str = include_str!("../docs/language/design-decisions.md");

#[test]
fn design_decisions_md_does_not_list_index_as_postfix_operator() {
    // Compile-time include keeps this anchored to the file the test
    // claims to lock. The runtime read is a redundant cross-check.
    let doc_runtime = read_doc(DOC_PATH);
    assert_eq!(
        DOC_SRC, doc_runtime,
        "include_str! and runtime read of {DOC_PATH} disagree"
    );

    // The misleading enumeration must be gone.
    assert!(
        !DOC_SRC.contains("function call, `?`, trailing closure, index"),
        "design-decisions.md must not enumerate `index` as a postfix \
         operator alongside function call / ? / trailing closure -- \
         silt's parser rejects `xs[i]` (it is reserved syntax, not a \
         real postfix operator)."
    );

    // Positive shape: the corrected enumeration (without `index`) is
    // present.
    assert!(
        DOC_SRC.contains("(function call, `?`, trailing closure)"),
        "design-decisions.md should still enumerate the real postfix \
         operators as `(function call, `?`, trailing closure)`"
    );

    // Replacement note must explain the reserved-but-unimplemented
    // status so the deletion of `index` doesn't drop the explanation.
    assert!(
        DOC_SRC.contains("Bracket indexing")
            && DOC_SRC.contains("reserved syntax")
            && DOC_SRC.contains("`list.get(xs, i)`"),
        "design-decisions.md should explicitly note that bracket \
         indexing `xs[i]` is reserved syntax (not a real postfix \
         operator) and point users at `list.get(xs, i)` etc."
    );
}

// ────────────────────────────────────────────────────────────────────
// Behavioral lock: the parser still rejects `xs[0]` with the
// documented `postfix indexing is not supported` diagnostic.
// ────────────────────────────────────────────────────────────────────

#[test]
fn silt_parser_rejects_bracket_indexing() {
    let dir = scratch_dir("index");
    let src = "fn main() {\n  let xs = [1, 2, 3]\n  let _r = xs[0]\n  ()\n}\n";
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "silt check should reject `xs[0]`; got success.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("postfix indexing is not supported"),
        "silt should emit the documented `postfix indexing is not \
         supported` error -- this is what makes the design-decisions.md \
         claim (that indexing is reserved, not a real postfix \
         operator) honest. Got stderr:\n{stderr}"
    );
    // Parity with the doc's recommended replacements.
    assert!(
        stderr.contains("list.get") && stderr.contains("string.char_at"),
        "the parser's diagnostic must still point at `list.get` and \
         `string.char_at`, matching the design-decisions.md note. Got \
         stderr:\n{stderr}"
    );
}
