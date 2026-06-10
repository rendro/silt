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

use std::path::{Path, PathBuf};

fn read_src(rel: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The bypass-shape source-grep used by round84's lock. Centralized
/// so the live `src/typechecker/` scan and the planted-offender unit
/// test use the *same* detection logic — if the detector regresses,
/// both call sites fail together.
const BYPASS_NEEDLE: &str = "self.errors.push(crate::types::TypeError {";

/// Recursively walk `root` and return every `(path, line_number)`
/// pair where `BYPASS_NEEDLE` appears inside a `.rs` file.
///
/// `files_scanned` is returned alongside so callers can sanity-check
/// that the walker actually reached subdirectories — without this,
/// the lock would silently pass if the recursion was broken or if
/// the typechecker layout changed.
fn find_offenders(root: &Path) -> (Vec<(PathBuf, usize)>, usize) {
    let mut offenders: Vec<(PathBuf, usize)> = Vec::new();
    let mut files_scanned: usize = 0;
    walk_rs(root, &mut offenders, &mut files_scanned);
    (offenders, files_scanned)
}

fn walk_rs(dir: &Path, offenders: &mut Vec<(PathBuf, usize)>, files_scanned: &mut usize) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let ft = entry.file_type().expect("file_type");
        if ft.is_dir() {
            walk_rs(&path, offenders, files_scanned);
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        *files_scanned += 1;
        for (i, line) in src.lines().enumerate() {
            if line.contains(BYPASS_NEEDLE) {
                offenders.push((path.clone(), i + 1));
            }
        }
    }
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
        n, 1,
        "expected exactly one `self.errors.push(` call site in \
         src/typechecker/inference.rs; found {n}. The single \
         sanctioned site (round 93) re-inserts pre-built TypeErrors \
         drained for match-arm dedup — see the documented call site. \
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
    let (offenders, files_scanned) = find_offenders(&dir);

    // Sentinel-count: there are 9 top-level files
    // (auto_derive/builtins/effects_infer/exhaustiveness/inference/
    // mod/resolve/suggest + 1) and 26 files in `builtins/`, totalling
    // 34+ `.rs` files at audit time. We assert a floor of 30 so the
    // lock fails loudly if the recursive walker regresses to
    // top-level-only (the round83 bug — `builtins/*.rs` was silently
    // exempt) or if the typechecker layout shrinks dramatically.
    // Floor is below current count to tolerate small refactors; it
    // is far enough above 9 (top-level only) that a regression to
    // non-recursive scanning trips the assert.
    assert!(
        files_scanned >= 30,
        "round 84 grep scanned only {files_scanned} files under {}; \
         expected >=30 (>=9 top-level + >=21 in `builtins/`). The \
         recursive walker may be broken, or the typechecker layout \
         changed enough that the lock no longer covers the intended \
         surface. If the shrink is intentional, lower this floor \
         deliberately.",
        dir.display()
    );
    assert!(
        offenders.is_empty(),
        "round 84 STYLE lock: the following typechecker modules \
         contain a direct `{BYPASS_NEEDLE}` construction; use the \
         `self.error(msg, span)` helper at \
         src/typechecker/mod.rs:2209 instead so Severity stays a \
         single point of truth. Offenders: {offenders:?}"
    );
}

/// Positive proof-of-lock: plant an offender file inside a
/// *subdirectory* of a tmp scratch root and assert
/// `find_offenders` actually finds it. Without the recursive
/// walker (the round83 bug), this test fails because the offender
/// lives one level below the scan root — exactly mirroring the
/// situation where `src/typechecker/builtins/*.rs` was silently
/// exempt from the live lock.
#[test]
fn round84_find_offenders_detects_planted_offender_in_subdir() {
    // Unique tmp root per test invocation so parallel test runs and
    // re-runs don't collide.
    let mut root = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    root.push(format!("silt_round84_proof_of_lock_{pid}_{nanos}"));
    // Clean up any prior leftover.
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create tmp root");

    // Subdir analogous to `src/typechecker/builtins/`.
    let subdir = root.join("builtins_like");
    std::fs::create_dir_all(&subdir).expect("create subdir");

    // Clean file at top level (to verify scan walks both layers).
    let clean_top = root.join("clean.rs");
    std::fs::write(&clean_top, "pub fn ok() { self.error(\"msg\", span); }\n")
        .expect("write clean_top");

    // Planted offender inside the subdirectory.
    let planted = subdir.join("offender.rs");
    let planted_body = "pub fn bad() {\n    \
         self.errors.push(crate::types::TypeError { \
         message: \"x\".into(), span, severity: \
         Severity::Error });\n}\n";
    std::fs::write(&planted, planted_body).expect("write planted offender");

    // Also plant a non-`.rs` file containing the needle — it must
    // be ignored by the extension filter.
    let decoy = subdir.join("offender.txt");
    std::fs::write(&decoy, planted_body).expect("write decoy");

    let (offenders, files_scanned) = find_offenders(&root);

    // Tear down before assertions so a failure doesn't leak the
    // tmpdir; copy out the data we need first.
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(
        files_scanned, 2,
        "expected to scan exactly 2 `.rs` files (1 clean top-level + \
         1 planted offender in subdir); scanned {files_scanned}. The \
         recursive walker is broken or the extension filter is wrong."
    );
    assert_eq!(
        offenders.len(),
        1,
        "expected exactly 1 planted offender, found {} -> {:?}",
        offenders.len(),
        offenders
    );
    let (off_path, off_line) = &offenders[0];
    assert!(
        off_path.ends_with("builtins_like/offender.rs"),
        "offender path mismatch: {}",
        off_path.display()
    );
    assert_eq!(
        *off_line, 2,
        "offender line mismatch (planted on line 2): got {off_line}"
    );
}
