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
