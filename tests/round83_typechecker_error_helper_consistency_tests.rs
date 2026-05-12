//! Round 83 audit — STYLE: typechecker error-emission consistency.
//!
//! Finding: two sites in `src/typechecker/inference.rs` (lines 892
//! and 3656 in the audit-time snapshot) constructed
//! `crate::types::TypeError { message, span, severity: Severity::Error }`
//! directly via `self.errors.push(...)`, bypassing the
//! `self.error(msg, span)` helper at `src/typechecker/mod.rs:2209`
//! that every other error site uses. silt's design principle is
//! "one way to do things" — equivalent dual shapes should collapse
//! to a single unified form. Round 83 rewrote those two sites to
//! call `self.error(...)`.
//!
//! This is a source-grep lock: it fails loudly if a future patch
//! reintroduces a direct `self.errors.push(crate::types::TypeError`
//! construction inside `inference.rs`. New error sites should go
//! through `self.error(...)` (or `self.warning(...)`) so the
//! Severity field stays a single point of truth.

use std::path::PathBuf;

fn read_src(rel: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// `src/typechecker/inference.rs` must not push raw `TypeError`
/// structs onto `self.errors` directly. All error emission goes
/// through the `self.error(msg, span)` helper.
#[test]
fn inference_does_not_push_raw_type_error_directly() {
    let src = read_src("src/typechecker/inference.rs");
    let needle = "self.errors.push(crate::types::TypeError";
    assert!(
        !src.contains(needle),
        "src/typechecker/inference.rs contains a direct \
         `{needle}` construction; use the `self.error(msg, span)` \
         helper at src/typechecker/mod.rs:2209 instead so Severity \
         stays a single point of truth (round 83 STYLE lock)."
    );
}

/// Belt-and-suspenders: count residual `self.errors.push(` lines
/// in inference.rs. After round 83 this should be 0; if a
/// legitimate future need emerges (e.g. pushing a pre-built
/// `TypeError` value forwarded from elsewhere), bump the threshold
/// here and document the reason at the new call site.
#[test]
fn inference_has_no_residual_errors_push_lines() {
    let src = read_src("src/typechecker/inference.rs");
    let n = src.matches("self.errors.push(").count();
    assert_eq!(
        n, 0,
        "expected zero `self.errors.push(` call sites in \
         src/typechecker/inference.rs after round 83; found {n}. \
         New error sites should use the `self.error(...)` helper."
    );
}

/// Round-84 extension: the bypass pattern is forbidden in *every*
/// `src/typechecker/*.rs` module, not just `inference.rs`. The
/// helper at `src/typechecker/mod.rs:2209` is the single source of
/// truth for emitting `Severity::Error`; sibling modules
/// (`resolve.rs`, `auto_derive.rs`, `effects_infer.rs`,
/// `exhaustiveness.rs`, `suggest.rs`, `builtins.rs`, and any future
/// additions) must funnel through it too.
///
/// We grep for the precise bypass shape
/// `self.errors.push(crate::types::TypeError {` rather than the bare
/// `self.errors.push(` — the helper's own definitions in `mod.rs`
/// legitimately call `self.errors.push(TypeError { … })` (sibling
/// path, no `crate::types::` prefix), and we don't want to flag
/// those.
#[test]
fn round84_typechecker_no_raw_type_error_push_in_any_module() {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.push("src/typechecker");
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));

    let needle = "self.errors.push(crate::types::TypeError {";
    let mut offenders: Vec<String> = Vec::new();
    let mut files_scanned: usize = 0;
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        // Skip subdirectories (e.g. `builtins/`); only top-level `.rs`
        // files are part of the typechecker proper.
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        files_scanned += 1;
        if src.contains(needle) {
            offenders.push(path.display().to_string());
        }
    }
    assert!(
        files_scanned > 0,
        "round 84 grep scanned 0 files in {}; the test would silently \
         pass if the typechecker module layout changes — fix the path.",
        dir.display()
    );
    assert!(
        offenders.is_empty(),
        "round 84 STYLE lock: the following typechecker modules \
         contain a direct `{needle}` construction; use the \
         `self.error(msg, span)` helper at \
         src/typechecker/mod.rs:2209 instead so Severity stays a \
         single point of truth. Offenders: {offenders:?}"
    );
}
