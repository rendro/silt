//! Round 93 regression: the compiler's tail-call flag must NOT leak
//! into statement-level head expressions.
//!
//! Root cause: `ExprKind::Block` sets `in_tail_position` before
//! compiling the last statement, but `compile_stmt` only intends
//! `Stmt::Expr` to inherit it (the expression IS the block result).
//! Pre-fix, the flag leaked into the first sub-expression compiled by
//! `Stmt::Let` (the bound value), `Stmt::WhenBool` (the condition),
//! and `Stmt::When` (the scrutinee). If that sub-expression was a
//! plain call, the compiler emitted `TailCall + argc + Return`,
//! REPLACING the current frame — everything after (the SetLocal for
//! `let`, the JumpIfFalse + else body for `when`) became dead code:
//!
//!   1. `fn f() { let x = g(41) }` returned 42 instead of `()`.
//!   2. `when check(n) else { panic(...) }` as the last statement
//!      returned the raw condition and silently skipped the panic.
//!   3. `when let Some(x) = find(n) else { panic(...) }` skipped both
//!      the pattern test and the else arm, leaking the scrutinee as
//!      the function's return value.
//!
//! These tests run real programs through the compiled `silt` binary
//! (compile + VM execution, not a typecheck-only lock) and also pin
//! that genuine TCO still works after the fix: deep tail recursion in
//! expression-statement position must complete without overflowing.

use std::process::Command;

/// Run a Silt source program via the CLI; return (stdout, stderr, success).
fn run_silt(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!(
        "silt_round93_tailstmt_{}_{}.silt",
        std::process::id(),
        label
    ));
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

// ── Failure mode 1: trailing `let` with a call value ─────────────────

/// A function whose last statement is `let x = g(41)` must evaluate to
/// `()`. Pre-fix the call compiled as TailCall and `g`'s result (42)
/// leaked out as `f`'s return value.
#[test]
fn trailing_let_call_value_returns_unit_not_callee_result() {
    let (stdout, stderr, ok) = run_silt(
        "let_call",
        r#"
fn g(n) {
  n + 1
}

fn f() {
  let x = g(41)
}

fn main() {
  let y = f()
  println("{y}")
}
"#,
    );
    assert!(ok, "expected success; stderr={stderr:?}");
    assert_eq!(
        stdout.trim(),
        "()",
        "f() must return unit, not g's result; stderr={stderr:?}"
    );
}

/// Same leak one level deeper: the trailing `let` lives in a match arm
/// block that is itself in tail position (Block -> Match arm -> Block
/// -> last Stmt::Let). The flag flows through every nesting level.
#[test]
fn trailing_let_in_nested_tail_match_arm_returns_unit() {
    let (stdout, stderr, ok) = run_silt(
        "let_nested",
        r#"
fn g(n) {
  n + 1
}

fn f(flag) {
  match flag {
    true -> {
      let x = g(41)
    }
    false -> {
      let y = g(98)
    }
  }
}

fn main() {
  println("{f(true)}")
  println("{f(false)}")
}
"#,
    );
    assert!(ok, "expected success; stderr={stderr:?}");
    assert_eq!(
        stdout.trim(),
        "()\n()",
        "both arms end in a trailing let; the arm value must be unit; stderr={stderr:?}"
    );
}

/// Destructuring-pattern variant of the same leak: `let (a, b) = pair(..)`
/// as the last statement must still bind and return `()`.
#[test]
fn trailing_let_destructuring_call_value_returns_unit() {
    let (stdout, stderr, ok) = run_silt(
        "let_destructure",
        r#"
fn pair(n) {
  (n, n + 1)
}

fn f() {
  let (a, b) = pair(1)
}

fn main() {
  println("{f()}")
}
"#,
    );
    assert!(ok, "expected success; stderr={stderr:?}");
    assert_eq!(stdout.trim(), "()", "stderr={stderr:?}");
}

// ── Failure mode 2: trailing `when <call> else { panic }` ────────────

/// When the condition holds, the trailing `when` must evaluate to `()`,
/// not to the raw condition value.
#[test]
fn trailing_when_bool_true_returns_unit() {
    let (stdout, stderr, ok) = run_silt(
        "when_true",
        r#"
fn check(n) {
  n > 0
}

fn f(n) {
  when check(n) else {
    panic("negative input")
  }
}

fn main() {
  let y = f(5)
  println("{y}")
}
"#,
    );
    assert!(ok, "expected success; stderr={stderr:?}");
    assert_eq!(
        stdout.trim(),
        "()",
        "when-statement must yield unit, not the condition; stderr={stderr:?}"
    );
}

/// When the condition fails, the else body MUST run. Pre-fix the
/// condition call compiled as TailCall+Return, so the JumpIfFalse and
/// the panic were dead code and the guard was silently skipped.
#[test]
fn trailing_when_bool_false_runs_else_panic() {
    let (stdout, stderr, ok) = run_silt(
        "when_false",
        r#"
fn check(n) {
  n > 0
}

fn f(n) {
  when check(n) else {
    panic("negative input")
  }
}

fn main() {
  let y = f(0 - 5)
  println("guard skipped, got {y}")
}
"#,
    );
    assert!(
        !ok,
        "the when-guard must panic for negative input; stdout={stdout:?}"
    );
    assert!(
        stderr.contains("negative input"),
        "panic message must surface; stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("guard skipped"),
        "code after f() must not run; stdout={stdout:?}"
    );
}

// ── Failure mode 3: trailing `when let <pat> = <call> else { panic }` ─

/// Pattern match succeeds: the statement yields `()` and the binding
/// works; the scrutinee must not leak out as the return value.
#[test]
fn trailing_when_let_match_returns_unit() {
    let (stdout, stderr, ok) = run_silt(
        "when_let_ok",
        r#"
fn find(n) {
  match n > 0 {
    true -> Some(n)
    false -> None
  }
}

fn f(n) {
  when let Some(x) = find(n) else {
    panic("not found")
  }
}

fn main() {
  let y = f(5)
  println("{y}")
}
"#,
    );
    assert!(ok, "expected success; stderr={stderr:?}");
    assert_eq!(
        stdout.trim(),
        "()",
        "when-let statement must yield unit, not Some(5); stderr={stderr:?}"
    );
}

/// Pattern match fails: the else body MUST run. Pre-fix the scrutinee
/// call compiled as TailCall+Return; the pattern test and else body
/// were dead code, and the raw `None` leaked as the return value.
#[test]
fn trailing_when_let_no_match_runs_else_panic() {
    let (stdout, stderr, ok) = run_silt(
        "when_let_fail",
        r#"
fn find(n) {
  match n > 0 {
    true -> Some(n)
    false -> None
  }
}

fn f(n) {
  when let Some(x) = find(n) else {
    panic("not found")
  }
}

fn main() {
  let y = f(0 - 5)
  println("guard skipped, got {y}")
}
"#,
    );
    assert!(
        !ok,
        "the when-let guard must panic when the pattern fails; stdout={stdout:?}"
    );
    assert!(
        stderr.contains("not found"),
        "panic message must surface; stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("guard skipped"),
        "code after f() must not run; stdout={stdout:?}"
    );
}

// ── TCO must still work after the fix ────────────────────────────────

/// Genuine tail position (expression-statement as block result) must
/// still compile to TailCall: 2,000,000 frames of recursion would blow
/// MAX_FRAMES (100k) without TCO.
#[test]
fn genuine_tail_recursion_still_eliminated() {
    let (stdout, stderr, ok) = run_silt(
        "tco_still_works",
        r#"
fn countdown(n) {
  match n {
    0 -> 0
    _ -> countdown(n - 1)
  }
}

fn main() {
  println("{countdown(2000000)}")
}
"#,
    );
    assert!(
        ok,
        "deep tail recursion must complete (TCO); stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("stack overflow"),
        "must not hit MAX_FRAMES; stderr={stderr:?}"
    );
    assert_eq!(stdout.trim(), "0");
}

/// A `let` followed by a tail call in the SAME block: the let's value
/// must not be tail-compiled, but the trailing expression statement
/// still must be — both properties in one function.
#[test]
fn let_then_tail_call_in_same_block() {
    let (stdout, stderr, ok) = run_silt(
        "let_then_tail",
        r#"
fn dec(n) {
  n - 1
}

fn countdown(n) {
  match n {
    0 -> 0
    _ -> {
      let m = dec(n)
      countdown(m)
    }
  }
}

fn main() {
  println("{countdown(2000000)}")
}
"#,
    );
    assert!(
        ok,
        "trailing expression statement keeps TCO; stderr={stderr:?}"
    );
    assert!(!stderr.contains("stack overflow"), "stderr={stderr:?}");
    assert_eq!(stdout.trim(), "0");
}
