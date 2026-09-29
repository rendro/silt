//! Round-93 BROKEN (soundness): `?` inside a lambda was type-unsound.
//!
//! The `ExprKind::QuestionMark` arm validates against
//! `self.current_return_type`, which only `check_fn_body_with_name`
//! set — the Lambda arm never established its own return-type
//! context, so `?` in a lambda was checked against the ENCLOSING
//! NAMED FN's return type. The VM (`Op::QuestionMark`) pops exactly
//! one frame — the lambda's — so the static check and the runtime
//! disagreed about which function `?` returns from.
//!
//! Two unsound layers, both closed in round 93:
//!   (a) when the ?-ed expression's type was an unsolved Var (the
//!       common case for an unannotated lambda param), the
//!       `Type::Var(_) => stay lenient` arm DROPPED the obligation
//!       entirely — now deferred to `finalize_deferred_checks`;
//!   (b) the Lambda arm now saves/sets/restores
//!       `current_return_type` around its body, mirroring
//!       `check_fn_body_with_name`.
//!
//! Pre-fix repro (both `silt check` exit 0):
//!   `let ys: List(Int) = xs |> list.map({ x -> x? + 1 })` over
//!   `[Ok(1), Err("bad"), Ok(3)]` printed `[2, Err(bad), 4]` — a
//!   List(Int) holding a Variant; folding it crashes at runtime on a
//!   statically-clean program.
//!
//! Real-path tests: every test shells out to the compiled CLI binary
//! (`check` for static assertions, `run` for execution-path
//! assertions), covering the parser → typechecker → compiler → VM
//! pipeline, not a synthetic AST.

use std::process::Command;

fn silt_cmd(label: &str, sub: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round93_lambda_qmark_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg(sub)
        .arg(&tmp)
        .output()
        .expect("spawn silt");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// `silt check`; return (stderr, success).
fn check_silt(label: &str, src: &str) -> (String, bool) {
    let (_, stderr, ok) = silt_cmd(label, "check", src);
    (stderr, ok)
}

/// `silt run`; assert success and return stdout.
fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = silt_cmd(label, "run", src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout:?}, stderr={stderr:?}"
    );
    stdout
}

// ── Static rejection of the unsound lambda ─────────────────────────

/// The verified pre-fix repro, concrete-annotated form: `silt check`
/// used to exit 0 and the program printed `[2, Err(bad), 4]` into a
/// List(Int).
#[test]
fn lambda_qmark_into_list_int_annotated_is_rejected() {
    let src = r#"
import list
fn main() {
  let xs = [Ok(1), Err("bad"), Ok(3)]
  let ys: List(Int) = xs |> list.map({ x -> x? + 1 })
  println(ys)
}
"#;
    let (stderr, ok) = check_silt("annotated", src);
    assert!(
        !ok,
        "lambda `?` unwrapping into List(Int) must be a static error; stderr={stderr:?}"
    );
    assert!(
        stderr.contains(
            "? operator can only be used inside a function that returns Result or Option"
        ),
        "expected the curated ?-context message; stderr={stderr:?}"
    );
}

/// Pipeline form without the let annotation: the lambda's return type
/// still resolves to Int via `x? + 1`, so the same rejection applies.
#[test]
fn lambda_qmark_pipeline_unannotated_is_rejected() {
    let src = r#"
import list
fn main() {
  let xs = [Ok(1), Err("bad"), Ok(3)]
  let ys = xs |> list.map({ x -> x? + 1 })
  println(ys)
}
"#;
    let (stderr, ok) = check_silt("pipeline", src);
    assert!(
        !ok,
        "lambda `?` + 1 must be a static error regardless of let annotation; stderr={stderr:?}"
    );
    assert!(
        stderr.contains(
            "? operator can only be used inside a function that returns Result or Option"
        ),
        "expected the curated ?-context message; stderr={stderr:?}"
    );
}

/// Layer (b) directly: a lambda whose param is concretely annotated —
/// the `?` sees a concrete Result immediately (no Var leniency) and
/// must be validated against the LAMBDA's return type, not main's.
#[test]
fn lambda_qmark_concrete_param_annotation_is_rejected() {
    let src = r#"
fn main() {
  let f = { x: Result(Int, String) -> x? + 1 }
  println(f(Ok(1)))
}
"#;
    let (stderr, ok) = check_silt("concrete_param", src);
    assert!(
        !ok,
        "`?` in an Int-returning lambda must be a static error; stderr={stderr:?}"
    );
}

/// The error must fire exactly once (pass-3 re-check used to be able
/// to double-report deferred obligations; `pending_question_marks` is
/// truncated alongside the other pools).
#[test]
fn lambda_qmark_rejection_reports_exactly_once() {
    let src = r#"
import list
fn main() {
  let xs = [Ok(1), Err("bad"), Ok(3)]
  let ys: List(Int) = xs |> list.map({ x -> x? + 1 })
  println(ys)
}
"#;
    let (stderr, ok) = check_silt("once", src);
    assert!(!ok);
    // Count rendered diagnostics, not raw substring occurrences — the
    // renderer repeats the message on the caret-label line.
    let n = stderr.matches("error[type]").count();
    assert_eq!(
        n, 1,
        "the lambda-? rejection must be reported exactly once, got {n}; stderr={stderr:?}"
    );
    assert!(
        stderr.contains(
            "? operator can only be used inside a function that returns Result or Option"
        ),
        "the single error must be the ?-context rejection; stderr={stderr:?}"
    );
}

// ── Valid lambda-`?` still typechecks AND runs ─────────────────────

/// A lambda that wraps the unwrapped value back into Ok returns a
/// Result, so `?` inside it is legal — and at runtime the VM pops the
/// lambda frame on Err, propagating per element through list.map.
#[test]
fn lambda_returning_result_with_qmark_typechecks_and_runs() {
    let src = r#"
import list
fn main() {
  let xs = [Ok(1), Err("bad"), Ok(3)]
  let ys = xs |> list.map({ x -> Ok(x? + 1) })
  println(ys)
}
"#;
    let (stderr, ok) = check_silt("valid_lambda_check", src);
    assert!(
        ok,
        "Result-returning lambda with `?` must typecheck; stderr={stderr:?}"
    );
    let out = run_silt_ok("valid_lambda_run", src);
    assert!(
        out.contains("[Ok(2), Err(bad), Ok(4)]"),
        "lambda `?` must propagate Err per element at runtime; stdout={out:?}"
    );
}

// ── `?` in named fns unaffected ────────────────────────────────────

/// Positive runtime test: the large existing named-fn `?` surface
/// keeps working — Result propagation.
#[test]
fn named_fn_qmark_result_still_typechecks_and_runs() {
    let src = r#"
fn safe_div(a: Int, b: Int) -> Result(Int, String) {
  match b == 0 {
    true -> Err("div by zero"),
    false -> Ok(a / b),
  }
}

fn compute() -> Result(Int, String) {
  let x = safe_div(10, 2)?
  Ok(x + 1)
}

fn fails() -> Result(Int, String) {
  let x = safe_div(10, 0)?
  Ok(x + 1)
}

fn main() {
  println(compute())
  println(fails())
}
"#;
    let out = run_silt_ok("named_fn_result", src);
    assert!(
        out.contains("Ok(6)"),
        "named-fn ? on the Ok path must unwrap; stdout={out:?}"
    );
    assert!(
        out.contains("Err(div by zero)"),
        "named-fn ? on the Err path must propagate; stdout={out:?}"
    );
}

/// Positive runtime test: Option propagation in a named fn.
#[test]
fn named_fn_qmark_option_still_typechecks_and_runs() {
    let src = r#"
fn find_positive(x: Int) -> Option(Int) {
  match x > 0 {
    true -> Some(x),
    false -> None,
  }
}

fn bump() -> Option(Int) {
  let v = find_positive(5)?
  Some(v + 1)
}

fn main() {
  println(bump())
}
"#;
    let out = run_silt_ok("named_fn_option", src);
    assert!(
        out.contains("Some(6)"),
        "named-fn ? over Option must unwrap; stdout={out:?}"
    );
}

/// A genuinely polymorphic named fn whose `?`-ed operand never
/// resolves stays lenient (Algorithm W: body vars never unify with
/// call sites) — the deferred check must not reject it.
#[test]
fn polymorphic_named_fn_with_qmark_stays_lenient() {
    let src = r#"
fn unwrap_plus(r) -> Result(Int, String) {
  let v = r?
  Ok(v + 1)
}

fn main() {
  println(unwrap_plus(Ok(1)))
}
"#;
    let (stderr, ok) = check_silt("poly_lenient", src);
    assert!(
        ok,
        "polymorphic named fn using ? must keep typechecking; stderr={stderr:?}"
    );
    let out = run_silt_ok("poly_lenient_run", src);
    assert!(out.contains("Ok(2)"), "stdout={out:?}");
}
