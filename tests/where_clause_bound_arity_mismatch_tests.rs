//! Round 101 regression lock: where-clause trait-bound arg ARITY is
//! checked in BOTH directions, at all three where-clause sites.
//!
//! Round 60 G1 only rejected the `trait_args.is_empty() &&
//! !params.is_empty()` direction, and only in
//! `check_fn_body_with_name`. A bound whose arg list was non-empty but
//! length-MISMATCHED (`where a: Cast(Int, String)` on a one-param
//! `trait Cast(to)`) sailed through: `verify_trait_obligation` only
//! runs its round-58 positional arg-compatibility zip when
//! `impl_args.len() == bound_trait_args.len()`, so the mismatched
//! bound silently degraded to a bare "implements Cast" check and
//! matched (and DISPATCHED through) any `Cast(*)` impl. At the
//! impl-level and method-level where-clause sites even the zero-args
//! direction was missing.
//!
//! The fix centralises the check in
//! `TypeChecker::check_where_bound_arity` (src/typechecker/mod.rs) and
//! applies it at the fn-level (`check_fn_body_with_name`), impl-level
//! and method-level (`register_trait_impl`) where-clause loops.
//!
//! Pre-fix, every `*_rejected` test below FAILS (the programs
//! typechecked clean; the two runtime repros printed "accepted" /
//! "1.5" and exited 0). The positive controls pin that matching arity
//! keeps working. See also `tests/where_clause_bound_arity_tests.rs`
//! (the original round-60 zero-args lock, still green).

use std::process::Command;

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Run a silt source program via the `silt run` subprocess and return
/// (stdout, stderr, success). Mirrors the helper in
/// `tests/trait_args_where_clause_runtime_tests.rs`.
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_where_arity_mismatch_{label}.silt"));
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

// ── (a) fn-level over-arity: the audit repro ────────────────────────

/// `where a: Cast(Int, String)` on `trait Cast(to)`: pre-fix the bound
/// degraded to a bare "implements Cast" check, the `Cast(Float) for
/// Int` impl satisfied it, and the program ran to completion printing
/// "accepted". Post-fix: arity error, and `silt run` must not succeed.
#[test]
fn fn_level_over_arity_bound_rejected() {
    let src = r#"
trait Cast(to) { fn cast(self) -> to }
trait Cast(Float) for Int { fn cast(self) -> Float { 1.0 } }
fn g(x: a) -> Float where a: Cast(Int, String) { 1.0 }
fn main() { let _ = g(42)
  println("accepted") }
"#;
    let errs = type_errors(src);
    let joined = errs.join("\n");
    assert!(
        joined.contains("trait 'Cast' expects 1 type argument in bound, got 2"),
        "expected over-arity bound error, got:\n{joined}"
    );

    // Execution-path lock: the pre-fix behaviour was a clean run that
    // printed "accepted" and exited 0.
    let (stdout, stderr, ok) = run_silt_raw("fn_level_over", src);
    assert!(
        !ok,
        "`silt run` MUST fail on the over-arity bound; \
         stdout: {stdout:?}, stderr: {stderr:?}"
    );
    assert!(
        !stdout.contains("accepted"),
        "program body must not execute; stdout: {stdout:?}"
    );
}

// ── (b) fn-level args on a 0-param trait ────────────────────────────

/// `where a: Named(Int, Bool)` on a parameterless trait used to be
/// silently accepted (the round-60 check only fired for the
/// zero-args-on-parameterized direction).
#[test]
fn fn_level_args_on_parameterless_trait_rejected() {
    let errs = type_errors(
        r#"
trait Named { fn name(self) -> String }
fn f(x: a) -> Int where a: Named(Int, Bool) { 1 }
fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("trait 'Named' expects 0 type arguments in bound, got 2"),
        "expected 0-param-trait arity error, got:\n{joined}"
    );
}

// ── (c) impl-level sites ────────────────────────────────────────────

/// Impl-level over-arity: pre-fix, `where a: Cast(Int, String)`
/// matched the `Cast(Float) for Int` impl bare, dispatched `.cast()`,
/// and printed 1.5 at runtime.
#[test]
fn impl_level_over_arity_bound_rejected() {
    let src = r#"
trait Cast(to) { fn cast(self) -> to }
trait Cast(Float) for Int { fn cast(self) -> Float { 1.5 } }
trait Use { fn use_it(self) -> Float }
trait Use for List(a) where a: Cast(Int, String) {
    fn use_it(self) -> Float = match self {
        [] -> 0.0
        [x, ..rest] -> x.cast() + rest.use_it()
    }
}
fn main() {
    let xs: List(Int) = [1]
    println(xs.use_it())
}
"#;
    let errs = type_errors(src);
    let joined = errs.join("\n");
    assert!(
        joined.contains("trait 'Cast' expects 1 type argument in bound, got 2"),
        "expected impl-level over-arity bound error, got:\n{joined}"
    );

    // Execution-path lock: pre-fix this ran and printed 1.5.
    let (stdout, stderr, ok) = run_silt_raw("impl_level_over", src);
    assert!(
        !ok,
        "`silt run` MUST fail on the impl-level over-arity bound; \
         stdout: {stdout:?}, stderr: {stderr:?}"
    );
    assert!(
        !stdout.contains("1.5"),
        "trait method must not dispatch; stdout: {stdout:?}"
    );
}

/// Impl-level bare bound on a parameterized trait: the round-60
/// zero-args direction never applied to `ti.where_clauses`.
#[test]
fn impl_level_bare_bound_on_parameterized_trait_rejected() {
    let errs = type_errors(
        r#"
trait Cast(to) { fn cast(self) -> to }
trait Use { fn use_it(self) -> Int }
trait Use for List(a) where a: Cast {
    fn use_it(self) -> Int = 0
}
fn main() { }
"#,
    );
    let joined = errs.join("\n");
    assert!(
        joined.contains("trait 'Cast' expects 1 type argument in bound, got 0"),
        "expected impl-level bare-bound arity error, got:\n{joined}"
    );
}

// ── method-level site ───────────────────────────────────────────────

/// Method-level over-arity bound rejects, and the diagnostic appears
/// exactly ONCE: the registration site and the method body's
/// `check_fn_body_with_name` both run the check, and
/// `check_where_bound_arity` dedupes on (message, span).
#[test]
fn method_level_over_arity_bound_rejected_once() {
    let errs = type_errors(
        r#"
trait Cast(to) { fn cast(self) -> to }
trait Use { fn use_it(self) -> Int }
trait Use for List(a) {
    fn use_it(self) -> Int where a: Cast(Int, String) = 0
}
fn main() { }
"#,
    );
    let hits = errs
        .iter()
        .filter(|e| e.contains("trait 'Cast' expects 1 type argument in bound, got 2"))
        .count();
    assert_eq!(
        hits,
        1,
        "expected exactly one method-level arity error (dedup), got {hits} in:\n{}",
        errs.join("\n")
    );
}

// ── (d) positive controls: matching arity keeps working ────────────

/// Impl-level matching arity still registers and runs end-to-end
/// (guards the `continue` in the fixed registration loop).
#[test]
fn impl_level_matching_arity_still_runs() {
    let src = r#"
trait Conv(to) { fn conv(self) -> to }
trait Conv(Int) for Int { fn conv(self) -> Int = self }
trait Use { fn use_it(self) -> Int }
trait Use for List(a) where a: Conv(Int) {
    fn use_it(self) -> Int = match self {
        [] -> 0
        [x, ..rest] -> x.conv() + rest.use_it()
    }
}
fn main() {
    let xs: List(Int) = [1, 2, 3]
    println(xs.use_it())
}
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "matching-arity impl-level bound must typecheck, got:\n{}",
        errs.join("\n")
    );
    let (stdout, stderr, ok) = run_silt_raw("impl_level_positive", src);
    assert!(ok, "positive control must run; stderr: {stderr:?}");
    assert!(
        stdout.contains('6'),
        "expected 6 on stdout, got: {stdout:?}"
    );
}
