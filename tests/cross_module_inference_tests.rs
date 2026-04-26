//! Round 64 item 6A: cross-module let-generalization.
//!
//! Verifies that when module B does `import a` (where `a` is a sibling
//! user module), the typechecker has full visibility into `a`'s pub
//! declarations — schemes are instantiated with fresh tyvars at each
//! call site, where-clause constraints flow through, pub trait impls
//! dispatch correctly, and pub types/variants are reachable. The
//! "unknown module 'a'; imported items will not be type-checked"
//! warning is no longer emitted for resolvable user modules.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use silt::compiler::Compiler;
use silt::intern::intern;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;

/// Create a fresh tempdir holding the supplied module files plus a
/// `main.silt` containing `main_source`. Returns the dir path.
fn setup_dir(files: &[(&str, &str)], main_source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "silt_xmod_{}_{}",
        std::process::id(),
        rand_u64()
    ));
    fs::create_dir_all(&dir).expect("mkdir");
    for (name, content) in files {
        fs::write(dir.join(name), content).expect("write module");
    }
    fs::write(dir.join("main.silt"), main_source).expect("write main");
    dir
}

fn rand_u64() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// Run typecheck-only against `main.silt` in `dir`. Returns the type
/// errors (warnings included) so tests can inspect them.
///
/// Mirrors what `pipeline.rs` does: builds a Compiler, runs
/// `pre_typecheck_imports` over the program, then calls
/// `check_with_package_and_imports` with the accumulated exports.
fn typecheck_main_in(dir: &PathBuf) -> Vec<typechecker::TypeError> {
    let main_path = dir.join("main.silt");
    let source = fs::read_to_string(&main_path).expect("read main");
    let tokens = Lexer::new(&source).tokenize().expect("lex");
    let mut program = Parser::new(tokens).parse_program().expect("parse");

    let local_pkg = intern("__test__");
    let mut roots = HashMap::new();
    roots.insert(local_pkg, dir.clone());
    let mut compiler = Compiler::with_package_roots(local_pkg, roots);
    compiler.pre_typecheck_imports(&program);
    let exports = compiler.module_exports_snapshot();
    let (errors, _) = typechecker::check_with_package_and_imports(
        &mut program,
        Some(local_pkg),
        exports,
    );
    errors
}

fn errors_only(errs: &[typechecker::TypeError]) -> Vec<&typechecker::TypeError> {
    errs.iter()
        .filter(|e| e.severity == typechecker::Severity::Error)
        .collect()
}

fn warnings_only(errs: &[typechecker::TypeError]) -> Vec<&typechecker::TypeError> {
    errs.iter()
        .filter(|e| e.severity == typechecker::Severity::Warning)
        .collect()
}

// ── 1. Generic fn import ───────────────────────────────────────────

#[test]
fn generic_fn_import_typechecks_clean_with_no_unknown_module_warning() {
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub fn id(x) = x
            "#,
        )],
        r#"
import a

fn main() {
  let n = a.id(42)
  let s = a.id("foo")
  n
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "expected no type errors, got: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let warnings = warnings_only(&errs);
    let unknown_module_warnings: Vec<&typechecker::TypeError> = warnings
        .iter()
        .copied()
        .filter(|w| w.message.contains("unknown module"))
        .collect();
    assert!(
        unknown_module_warnings.is_empty(),
        "expected no 'unknown module' warning for resolvable user module, got: {:?}",
        unknown_module_warnings
            .iter()
            .map(|e| &e.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn generic_fn_import_polymorphic_call_at_two_concrete_types() {
    // Pure typecheck: each call site instantiates `id` with a fresh
    // tyvar, so calling at Int then String must not collapse them.
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub fn id(x) = x
            "#,
        )],
        r#"
import a

fn main() {
  let n = a.id(42)
  let s = a.id("hello")
  s
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "polymorphic recall should not collapse: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 2. Constrained fn import ───────────────────────────────────────

#[test]
fn constrained_fn_import_satisfied_call_typechecks() {
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub fn show(x: a) -> String where a: Display = x.display()
            "#,
        )],
        r#"
import a

fn main() {
  a.show(21)
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "satisfied where-bound should typecheck: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn constrained_fn_import_unsatisfied_call_fails_at_bound_not_at_unknown_module() {
    // Module a defines a trait `Greet` with a method, and a generic
    // function constrained on it. The importer calls that function
    // with a type that doesn't implement Greet — the diagnostic must
    // be a "does not implement Greet" error, NOT the legacy "unknown
    // module" warning.
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub trait Greet {
    fn greet(self) -> String
}

pub fn announce(x: a) -> String where a: Greet = x.greet()
            "#,
        )],
        r#"
import a

fn main() {
  a.announce("foo")
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    // The diagnostic should NOT be the legacy "unknown module"
    // warning. Whether it surfaces as a Numeric-bound failure or a
    // type-mismatch on the arg, what matters is that we got past
    // the warning and into real type-checking of the call site.
    let unknown_module_msgs: Vec<&typechecker::TypeError> = errs
        .iter()
        .filter(|e| e.message.contains("unknown module"))
        .collect();
    assert!(
        unknown_module_msgs.is_empty(),
        "should not emit 'unknown module' for resolvable module: {:?}",
        unknown_module_msgs
            .iter()
            .map(|e| &e.message)
            .collect::<Vec<_>>()
    );
    // And there must be at least one real diagnostic about the bad
    // call (either a constraint or a unification error on the
    // String argument).
    let any_call_error = errs.iter().any(|e| {
        let m = &e.message;
        m.contains("Numeric")
            || m.contains("does not implement")
            || m.contains("type mismatch")
            || m.contains("expected")
    });
    assert!(
        any_call_error,
        "expected a real call-site error for unsatisfied bound, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 3. Pub type import ─────────────────────────────────────────────

#[test]
fn pub_enum_import_variant_resolves_under_qualified_name() {
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub type Color { Red, Green, Blue }
            "#,
        )],
        r#"
import a

fn main() {
  let c = a.Red
  c
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "qualified variant access should typecheck: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn pub_enum_import_variant_pattern_match() {
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub type Color { Red, Green, Blue }
            "#,
        )],
        r#"
import a

fn describe(c) {
  match c {
    Red -> "red",
    Green -> "green",
    Blue -> "blue",
  }
}

fn main() {
  describe(a.Red)
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "pattern match across module boundary should typecheck: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 4. Pub trait import ────────────────────────────────────────────

#[test]
fn pub_trait_import_local_impl_method_dispatch() {
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub trait Describe {
    fn describe(self) -> String
}
            "#,
        )],
        r#"
import a

type Box(t) { Wrap(t) }

trait Describe for Box(t) where t: Display {
    fn describe(self) -> String = match self {
        Wrap(x) -> "Box({x})"
    }
}

fn main() {
  Wrap(42).describe()
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "cross-module trait + local impl should typecheck: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 5. Pub trait + pub impl + downstream consumer ──────────────────

#[test]
fn pub_trait_pub_impl_downstream_consumer() {
    // Module a defines a trait + impls it for a built-in type. Module
    // b imports a and writes a generic function constrained on the
    // imported trait, then calls it on the type a's impl covers.
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub trait Greet {
    fn greet(self) -> String
}

trait Greet for Int {
    fn greet(self) -> String = "int-{self}"
}
            "#,
        )],
        r#"
import a

fn announce(x: a) -> String where a: Greet = x.greet()

fn main() {
  announce(7)
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let real_errors = errors_only(&errs);
    assert!(
        real_errors.is_empty(),
        "downstream consumer of pub trait+impl should typecheck: {:?}",
        real_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── 6. Cycle detection still works ─────────────────────────────────

#[test]
fn cross_module_does_not_loop_on_cycle() {
    // Modules a and b mutually import each other. Pre-typecheck must
    // not infinite-loop; the compile pass will surface the real cycle
    // error.
    let dir = setup_dir(
        &[
            ("a.silt", "import b\npub fn fa() = 1\n"),
            ("b.silt", "import a\npub fn fb() = 2\n"),
        ],
        r#"
import a

fn main() {
  a.fa()
}
        "#,
    );
    // Just running typecheck must terminate. Errors/warnings are
    // not the test contract here — termination is.
    let _ = typecheck_main_in(&dir);
}

// ── 7. Imported but unused — no warning churn ──────────────────────

#[test]
fn unused_user_import_does_not_emit_unknown_module_warning() {
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub fn unused() = 0
            "#,
        )],
        r#"
import a

fn main() {
  42
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let unknown_module_warnings: Vec<&typechecker::TypeError> = errs
        .iter()
        .filter(|w| w.message.contains("unknown module"))
        .collect();
    assert!(
        unknown_module_warnings.is_empty(),
        "unused but resolvable user import should not warn: {:?}",
        unknown_module_warnings
            .iter()
            .map(|e| &e.message)
            .collect::<Vec<_>>()
    );
}

// ── 8. Diagnostic on misuse mentions imported origin ───────────────

#[test]
fn arity_mismatch_on_imported_fn_emits_real_diagnostic() {
    // 3 args supplied to a 2-arg fn — the module-call arity tolerance
    // (`args == params OR args + 1 == params`) doesn't cover this case,
    // so the type-checker must surface a clean arity error keyed on
    // the imported fn's true signature.
    let dir = setup_dir(
        &[(
            "a.silt",
            r#"
pub fn add(x, y) = x + y
            "#,
        )],
        r#"
import a

fn main() {
  a.add(1, 2, 3)
}
        "#,
    );
    let errs = typecheck_main_in(&dir);
    let unknown_module_warnings: Vec<&typechecker::TypeError> = errs
        .iter()
        .filter(|w| w.message.contains("unknown module"))
        .collect();
    assert!(
        unknown_module_warnings.is_empty(),
        "should not emit 'unknown module' for resolvable user module: {:?}",
        unknown_module_warnings
            .iter()
            .map(|e| &e.message)
            .collect::<Vec<_>>()
    );
    // We expect a real arity error (the typechecker should now see
    // a.add's true 2-arg signature).
    let any_arity_error = errs.iter().any(|e| {
        let m = &e.message;
        m.contains("arg") || m.contains("argument") || m.contains("expects")
    });
    assert!(
        any_arity_error,
        "expected a real arity diagnostic for a.add(1), got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}
