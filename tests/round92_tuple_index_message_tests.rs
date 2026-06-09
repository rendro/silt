//! Round-92 diagnostic-quality lock: `t.0` on a tuple.
//!
//! The parser has a dedicated tuple-index arm, so `t.0` reaches the
//! typechecker as a `FieldAccess` whose "method" name is the literal
//! `0`. Tuple indexing is intentionally unsupported (round-69
//! decision — destructuring is the one way), but the pre-fix
//! diagnostic was the generic container fallback
//!
//!   unknown method '0' on Tuple
//!
//! which reads like a typo'd method name rather than an unsupported
//! feature. Round 92 emits a targeted message in the established hint
//! style ("silt has no 'if' keyword — …", "postfix indexing is not
//! supported; use list.get(xs, i), …") pointing at destructuring.
//! Tuple indexing support itself remains a deferred design decision.
//!
//! Real-path tests: each one shells out to the compiled CLI binary and
//! checks an actual program, so the assertion covers the parser →
//! typechecker pipeline, not a synthetic AST.

use std::process::Command;

/// `silt check` the given source; return (stderr, success).
fn check_silt(label: &str, src: &str) -> (String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round92_tuple_idx_{label}.silt"));
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

#[test]
fn tuple_index_gets_targeted_message_not_unknown_method() {
    let src = r#"
fn main() {
  let t = (1, 2)
  println(t.0)
}
"#;
    let (stderr, ok) = check_silt("basic", src);
    assert!(!ok, "t.0 must still be rejected; stderr={stderr:?}");
    assert!(
        stderr.contains("tuple indexing ('t.0') is not supported"),
        "expected the targeted tuple-indexing message; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("destructure instead: 'let (a, b) = t'"),
        "expected the destructuring hint; stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("unknown method '0'"),
        "the misleading unknown-method wording must be gone; stderr={stderr:?}"
    );
}

/// Multi-digit indices take the same path (the parser interns the int
/// token's decimal rendering as the field name).
#[test]
fn tuple_index_multi_digit_gets_targeted_message() {
    let src = r#"
fn main() {
  let t = (1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12)
  println(t.11)
}
"#;
    let (stderr, ok) = check_silt("multi_digit", src);
    assert!(!ok, "t.11 must still be rejected; stderr={stderr:?}");
    assert!(
        stderr.contains("tuple indexing ('t.11') is not supported"),
        "expected the targeted message for t.11; stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("unknown method '11'"),
        "the misleading unknown-method wording must be gone; stderr={stderr:?}"
    );
}

/// Regression guard: a *named* unknown method on a tuple must keep the
/// generic unknown-method diagnostic — the targeted message is scoped
/// to all-numeric names only (which can only arise from tuple-index
/// syntax).
#[test]
fn named_unknown_method_on_tuple_keeps_generic_message() {
    let src = r#"
fn main() {
  let t = (1, 2)
  println(t.frobnicate())
}
"#;
    let (stderr, ok) = check_silt("named_method", src);
    assert!(!ok, "t.frobnicate() must be rejected; stderr={stderr:?}");
    assert!(
        stderr.contains("unknown method 'frobnicate'"),
        "named methods keep the generic wording; stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("tuple indexing"),
        "the tuple-indexing hint must not fire for named methods; stderr={stderr:?}"
    );
}
