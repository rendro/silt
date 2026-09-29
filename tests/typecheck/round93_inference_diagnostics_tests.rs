//! Round-93 diagnostic-quality locks for typechecker inference.
//!
//! Three GAPs closed:
//!
//! 1. Map/Set literal element-mismatch diagnostics were
//!    direction-reversed: the unify calls passed the established type
//!    as t1 ("got") and the deviant element as t2 ("expected"),
//!    violating the documented t1=got/t2=expected convention.
//!    `#[1, 2, "x"]` reported "expected String, got Int". Round 93
//!    gives sets and maps the curated message list literals already
//!    had ("list elements must have the same type: first element is
//!    Int, but element 3 is String"), mirroring its wording exactly.
//!
//! 2. A match against the wrong scrutinee type emitted the identical
//!    diagnostic once per arm plus a "non-exhaustive match" cascade.
//!    Round 93 dedups identical (message, span) diagnostics from the
//!    arm loop and suppresses the exhaustiveness cascade when the
//!    scrutinee type already failed to match the arms. Genuinely
//!    distinct per-arm errors still each report, and genuine
//!    non-exhaustiveness still fires.
//!
//! 3. `?` in an unannotated (effectively unit-returning) fn surfaced
//!    as a bare header-located "type mismatch: expected Result(_,
//!    String), got ()" with no pointer to the `?`. Round 93 appends a
//!    note naming the `?` site and the return type it demands.
//!
//! Real-path tests: each test shells out to the compiled CLI binary
//! so the assertions cover the full rendered diagnostic output.

use std::process::Command;

/// `silt check` the given source; return (stderr, success).
fn check_silt(label: &str, src: &str) -> (String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round93_inference_diag_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("check")
        .arg(&tmp)
        .output()
        .expect("spawn silt check");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stderr, out.status.success())
}

// ── Collection-literal mismatch wording (direction + curation) ─────

/// Baseline: the list wording the set/map messages mirror.
#[test]
fn list_element_mismatch_names_first_and_deviant_in_order() {
    let (stderr, ok) = check_silt(
        "list",
        r#"
fn main() {
  let xs = [1, 2, "x"]
  println(xs)
}
"#,
    );
    assert!(
        !ok,
        "mixed list literal must be rejected; stderr={stderr:?}"
    );
    assert!(
        stderr.contains(
            "list elements must have the same type: first element is Int, but element 3 is String"
        ),
        "list wording/direction drifted; stderr={stderr:?}"
    );
}

#[test]
fn set_element_mismatch_uses_curated_message_with_correct_direction() {
    let (stderr, ok) = check_silt(
        "set",
        r#"
fn main() {
  let s = #[1, 2, "x"]
  println(s)
}
"#,
    );
    assert!(!ok, "mixed set literal must be rejected; stderr={stderr:?}");
    assert!(
        stderr.contains(
            "set elements must have the same type: first element is Int, but element 3 is String"
        ),
        "expected the curated set message (Int established, String deviant); stderr={stderr:?}"
    );
    // The pre-fix reversed raw mismatch must be gone.
    assert!(
        !stderr.contains("expected String, got Int"),
        "the direction-reversed raw mismatch must be gone; stderr={stderr:?}"
    );
}

#[test]
fn map_key_mismatch_uses_curated_message_with_correct_direction() {
    let (stderr, ok) = check_silt(
        "map_key",
        r#"
fn main() {
  let m = #{ "a": 1, 2: 3 }
  println(m)
}
"#,
    );
    assert!(!ok, "mixed map keys must be rejected; stderr={stderr:?}");
    assert!(
        stderr.contains("map keys must have the same type: first key is String, but key 2 is Int"),
        "expected the curated map-key message (String established, Int deviant); stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("expected Int, got String"),
        "the direction-reversed raw mismatch must be gone; stderr={stderr:?}"
    );
}

#[test]
fn map_value_mismatch_uses_curated_message_with_correct_direction() {
    let (stderr, ok) = check_silt(
        "map_value",
        r#"
fn main() {
  let m = #{ "a": 1, "b": "x" }
  println(m)
}
"#,
    );
    assert!(!ok, "mixed map values must be rejected; stderr={stderr:?}");
    assert!(
        stderr.contains(
            "map values must have the same type: first value is Int, but value 2 is String"
        ),
        "expected the curated map-value message (Int established, String deviant); stderr={stderr:?}"
    );
}

// ── Match wrong-scrutinee dedup + cascade suppression ──────────────

/// `match 42 { Ok(v) -> ..., Err(e) -> ... }` used to print the same
/// "expected Result(_, _), got Int" TWICE plus a non-exhaustive
/// cascade. Now: exactly ONE type error, NO cascade.
#[test]
fn match_wrong_scrutinee_emits_exactly_one_error_and_no_cascade() {
    let (stderr, ok) = check_silt(
        "match_wrong_scrutinee",
        r#"
fn main() {
  let r = match 42 {
    Ok(v) -> v,
    Err(e) -> 0,
  }
  println(r)
}
"#,
    );
    assert!(
        !ok,
        "wrong-scrutinee match must be rejected; stderr={stderr:?}"
    );
    // Count rendered diagnostics, not raw substring occurrences — the
    // renderer repeats the message on the caret-label line.
    let n = stderr.matches("error[type]").count();
    assert_eq!(
        n, 1,
        "identical per-arm diagnostics must be deduped to exactly one error, got {n}; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("expected Result(_, _), got Int"),
        "the single error must be the scrutinee mismatch; stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("non-exhaustive"),
        "the exhaustiveness cascade must be suppressed when the scrutinee \
         already failed to match the arms; stderr={stderr:?}"
    );
}

/// Genuinely distinct per-arm errors must still each report — dedup
/// keys on (message, span), not on "first error wins".
#[test]
fn match_distinct_arm_errors_still_each_report() {
    let (stderr, ok) = check_silt(
        "match_distinct_arms",
        r#"
fn main() {
  let r = match 42 {
    Ok(v) -> v,
    "s" -> 1,
    _ -> 0,
  }
  println(r)
}
"#,
    );
    assert!(!ok, "broken match must be rejected; stderr={stderr:?}");
    assert!(
        stderr.contains("Result"),
        "the Ok-arm mismatch must still report; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("String"),
        "the string-literal-arm mismatch must still report; stderr={stderr:?}"
    );
}

/// Guard against over-suppression: a well-typed but incomplete match
/// must still get its non-exhaustive diagnostic.
#[test]
fn genuine_non_exhaustive_match_still_reported() {
    let (stderr, ok) = check_silt(
        "genuine_nonexhaustive",
        r#"
fn main() {
  let r = match Ok(1) {
    Ok(v) -> v,
  }
  println(r)
}
"#,
    );
    assert!(!ok, "incomplete match must be rejected; stderr={stderr:?}");
    assert!(
        stderr.contains("non-exhaustive"),
        "genuine non-exhaustiveness must still fire; stderr={stderr:?}"
    );
}

// ── `?` in a unit/unannotated fn points at the `?` site ────────────

#[test]
fn qmark_in_unannotated_fn_diagnostic_names_the_qmark_site() {
    let (stderr, ok) = check_silt(
        "qmark_unit_fn",
        r#"
fn parse() -> Result(Int, String) {
  Ok(1)
}

fn get() {
  let x = parse()?
  println(x)
}

fn main() {
  get()
}
"#,
    );
    assert!(
        !ok,
        "? in a unit-returning fn must be rejected; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("the ? on line 7 requires this function to return Result(_, String)"),
        "the mismatch must carry a note pointing at the ? site; stderr={stderr:?}"
    );
    // The raw mismatch itself is still the headline (header-located),
    // the note is a continuation line.
    assert!(
        stderr.contains("expected Result(_, String), got ()"),
        "the underlying mismatch must still be reported; stderr={stderr:?}"
    );
}
