//! Regression tests for trait-method-on-constrained-TyVar instantiation.
//!
//! When a FieldAccess resolves `x.method()` on a receiver of type
//! `Type::Var(v)` with an active trait constraint, the method type is
//! looked up in `TraitInfo.methods`. Those types contain TyVars allocated
//! once at `register_trait_decl` time and shared across every call site.
//!
//! Without instantiation, downstream unification at the `Call` arm binds
//! those shared template TyVars in `self.subst`. A second constrained
//! call site that unifies a different concrete type against the same
//! template TyVar sees the first site's binding instead of a polymorphic
//! var — surfacing as a spurious "type mismatch" error.
//!
//! The fix in src/typechecker/inference.rs near line 1480 replaces
//! `self.apply(method_ty)` with `self.instantiate_method_type(method_ty)`
//! so each resolution gets fresh TyVars, mirroring what method_table
//! dispatch already does for concrete receivers.

use silt::typechecker;
use silt::types::Severity;

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Primary regression — trait method has a polymorphic return type
/// (not tied to Self), and two constrained call sites resolve it to
/// different concrete types. Without the fix, the first site's
/// unification permanently bound the shared template TyVar, and the
/// second site saw the first site's concrete type.
///
/// Pre-fix: `use_str` body emits "type mismatch: expected String, got Int".
/// Post-fix: both sites typecheck cleanly and run to produce their
/// respective return values.
#[test]
fn test_trait_method_polymorphic_return_two_sites_different_types() {
    let errs = type_errors(
        r#"
trait Container {
  fn get_val(self) -> a
}

type IntBox { v: Int }
type StrBox { v: String }

trait Container for IntBox {
  fn get_val(self) -> Int { self.v }
}
trait Container for StrBox {
  fn get_val(self) -> String { self.v }
}

fn use_int(c: x) -> Int where x: Container { c.get_val() }
fn use_str(c: y) -> String where y: Container { c.get_val() }

fn main() {
  let a = IntBox { v: 42 }
  let b = StrBox { v: "hello" }
  println(use_int(a))
  println(use_str(b))
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors with polymorphic-return trait method across two constrained sites, got: {errs:?}"
    );
}

/// End-to-end runtime check: the same program must run and print both
/// values. Catches unsoundness where the typechecker passes but dispatch
/// picks the wrong impl for one of the sites.
#[test]
fn test_trait_method_polymorphic_return_runtime_dispatch_correct() {
    use std::process::Command;
    let src = r#"
trait Container {
  fn get_val(self) -> a
}

type IntBox { v: Int }
type StrBox { v: String }

trait Container for IntBox {
  fn get_val(self) -> Int { self.v }
}
trait Container for StrBox {
  fn get_val(self) -> String { self.v }
}

fn use_int(c: x) -> Int where x: Container { c.get_val() }
fn use_str(c: y) -> String where y: Container { c.get_val() }

fn main() {
  let a = IntBox { v: 42 }
  let b = StrBox { v: "hello" }
  println(use_int(a))
  println(use_str(b))
}
"#;
    let tmp = std::env::temp_dir().join("silt_trait_poly_return_runtime.silt");
    std::fs::write(&tmp, src).expect("write temp file");

    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("run silt");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "silt run should succeed; stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stdout.contains("42") && stdout.contains("hello"),
        "expected both '42' and 'hello' in stdout; got: {stdout}"
    );
}

/// Negative lock: passing a non-implementing type to a constrained fn
/// must still be rejected after the instantiation change. If the fix
/// accidentally created a fresh-var hole that swallows the constraint
/// check, this would start passing silently.
#[test]
fn test_trait_method_constrained_tyvar_rejects_non_implementor() {
    let errs = type_errors(
        r#"
trait Measurable {
  fn measure(self) -> Int
}
trait Measurable for String {
  fn measure(self) -> Int { 0 }
}
fn measure_it(x: a) -> Int where a: Measurable { x.measure() }
fn main() { println(measure_it(42)) }
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Measurable'")),
        "expected 'does not implement trait Measurable' error, got: {errs:?}"
    );
}

/// Non-polymorphic return (monomorphic method) still works correctly.
/// Confirms the fix doesn't break the simple case.
#[test]
fn test_trait_method_monomorphic_return_two_sites_different_types() {
    let errs = type_errors(
        r#"
trait Summarizable {
  fn summarize(self) -> String
}
trait Summarizable for Int {
  fn summarize(self) -> String { "int" }
}
trait Summarizable for String {
  fn summarize(self) -> String { self }
}
fn show_a(x: a) -> String where a: Summarizable { x.summarize() }
fn show_b(y: b) -> String where b: Summarizable { y.summarize() }
fn main() {
  println(show_a(42))
  println(show_b("hello"))
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no type errors for monomorphic-return trait method, got: {errs:?}"
    );
}
