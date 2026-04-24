//! Regression tests for round 60 / B4 — `Op::Negate` overflow message
//! must not double-print the leading minus when negating `i64::MIN`.
//!
//! Background: `src/vm/execute.rs` previously formatted the overflow as
//! `format!("integer overflow: -{n}")`. When `n == i64::MIN`, the
//! `Display` impl on `n` already emits the leading `-`, so the message
//! collapsed to `integer overflow: --9223372036854775808` (double
//! minus). The fix rewrites the format as
//! `format!("integer overflow: negate {n}")`, mirroring the
//! `integer overflow: {a} + {b}` shape in `src/vm/arithmetic.rs:18-43`.
//!
//! Both a SOURCE-GREP lock (catches anyone reintroducing the bad
//! template) and a behavioral repro (runs the silt CLI on the canonical
//! source from the audit finding) guard against regression.
//!
//! NOTE: per round-60 hard rules, only `src/vm/execute.rs` is touched
//! by this fix; `src/vm/arithmetic.rs` is owned by another lane and we
//! consciously do NOT format negate as `0 - {n}`.
//!
//! Repro source (from finding B4):
//! ```silt
//! fn main() {
//!     let x: Int = 0 - 9223372036854775807 - 1
//!     let y = -x
//!     println(y)
//! }
//! ```
//! Pre-fix message: `error[runtime]: integer overflow: --9223372036854775808`
//! Post-fix message: `error[runtime]: integer overflow: negate -9223372036854775808`

use std::process::Command;

/// Lock 1 — source-grep guard. The bad format-string template
/// `integer overflow: -{` (the `{` confirms it's a `format!` arg, not
/// a code-point literal) must not reappear in `src/vm/execute.rs`.
#[test]
fn execute_rs_does_not_contain_double_minus_template() {
    let src = include_str!("../src/vm/execute.rs");
    assert!(
        !src.contains("integer overflow: -{"),
        "src/vm/execute.rs reintroduced the buggy `integer overflow: -{{n}}` \
         template — when n == i64::MIN, Display already prints the leading \
         minus, producing `--9223372036854775808`. Use \
         `integer overflow: negate {{n}}` instead."
    );
    // Positive assertion: the new template must be present so the test
    // catches a stealth removal of the fix as well.
    assert!(
        src.contains("integer overflow: negate {n}"),
        "expected the post-fix `integer overflow: negate {{n}}` template \
         in src/vm/execute.rs; B4 fix appears to have been removed."
    );
}

/// Run a Silt source program via the `silt` CLI and return
/// (stdout, stderr, success) — pattern borrowed from
/// tests/aliased_import_runtime_tests.rs.
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_negate_imin_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// Lock 2 — behavioral repro. Negating `i64::MIN` must surface a
/// runtime error whose message contains the new wording AND does NOT
/// contain the double-minus artifact `--9223372036854775808`.
#[test]
fn negate_i64_min_emits_clean_message() {
    let (stdout, stderr, ok) = run_silt_raw(
        "imin",
        r#"
fn main() {
    let x: Int = 0 - 9223372036854775807 - 1
    let y = -x
    println(y)
}
"#,
    );
    assert!(
        !ok,
        "negating i64::MIN must fail at runtime; stdout={stdout:?}, stderr={stderr:?}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        !combined.contains("--9223372036854775808"),
        "negate-overflow message must not contain double-minus artifact \
         `--9223372036854775808`; got: {combined}"
    );
    assert!(
        combined.contains("integer overflow: negate -9223372036854775808"),
        "expected `integer overflow: negate -9223372036854775808` in \
         output; got: {combined}"
    );
}
