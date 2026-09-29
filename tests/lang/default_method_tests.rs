//! Default-method-body tests (`trait X { fn f(self) -> T { default } }`).
//!
//! Covers:
//! 1. Default method used at runtime when impl omits it.
//! 2. Impl override of a default method takes effect at runtime.
//! 3. Mix of default and abstract methods — abstract must be implemented;
//!    default may be omitted.
//! 4. Missing non-default (abstract) method still errors.
//! 5. Default method body can call other trait methods on self.
//! 6. Default method on a parameterized impl target (`trait X for Box(a)`).
//! 7. Formatter roundtrip preserves default bodies.
//! 8. Default method that calls a supertrait method
//!    (interaction with the supertrait feature).

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

/// Run a Silt program end-to-end via the CLI and return its stdout.
/// Asserts the run succeeded.
fn run_silt(label: &str, src: &str) -> String {
    use std::process::Command;
    let tmp = std::env::temp_dir().join(format!("silt_default_method_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("run silt");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

/// 1. Default method body is used when the impl omits the method.
///    Typecheck should be clean and the runtime should print the
///    default's return value.
#[test]
fn test_default_method_used_when_impl_omits() {
    let errs = type_errors(
        r#"
trait Greeter {
  fn greet(self) -> String { "default-hello" }
}

type Item { v: Int }

trait Greeter for Item {}

fn main() {
  let it = Item { v: 1 }
  println(it.greet())
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected impl that omits a defaulted method to typecheck cleanly, got: {errs:?}"
    );

    let stdout = run_silt(
        "default_used",
        r#"
trait Greeter {
  fn greet(self) -> String { "default-hello" }
}

type Item { v: Int }

trait Greeter for Item {}

fn main() {
  let it = Item { v: 1 }
  println(it.greet())
}
"#,
    );
    assert!(
        stdout.contains("default-hello"),
        "expected default body to be invoked at runtime; stdout={stdout}"
    );
}

/// 2. The impl can override a default method; the override takes effect
///    at runtime instead of the default.
#[test]
fn test_impl_overrides_default_method() {
    let errs = type_errors(
        r#"
trait Greeter {
  fn greet(self) -> String { "default-hello" }
}

type Item { v: Int }

trait Greeter for Item {
  fn greet(self) -> String { "explicit-impl" }
}

fn main() {
  let it = Item { v: 1 }
  println(it.greet())
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected override of defaulted method to typecheck, got: {errs:?}"
    );

    let stdout = run_silt(
        "default_override",
        r#"
trait Greeter {
  fn greet(self) -> String { "default-hello" }
}

type Item { v: Int }

trait Greeter for Item {
  fn greet(self) -> String { "explicit-impl" }
}

fn main() {
  let it = Item { v: 1 }
  println(it.greet())
}
"#,
    );
    assert!(
        stdout.contains("explicit-impl") && !stdout.contains("default-hello"),
        "expected explicit override to be invoked, not the default; stdout={stdout}"
    );
}

/// 3. Mix of default + abstract methods. The abstract method MUST be
///    implemented; the default MAY be omitted. Both should compose
///    cleanly.
#[test]
fn test_mix_default_and_abstract_methods() {
    let errs = type_errors(
        r#"
trait Display2 {
  fn show(self) -> String { "default-show" }
  fn debug(self) -> String
}

type Item { v: Int }

trait Display2 for Item {
  fn debug(self) -> String { "item-debug" }
}

fn main() {
  let it = Item { v: 1 }
  println(it.show())
  println(it.debug())
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected mix of default + abstract (with abstract implemented) to typecheck, got: {errs:?}"
    );

    let stdout = run_silt(
        "mix_default_abstract",
        r#"
trait Display2 {
  fn show(self) -> String { "default-show" }
  fn debug(self) -> String
}

type Item { v: Int }

trait Display2 for Item {
  fn debug(self) -> String { "item-debug" }
}

fn main() {
  let it = Item { v: 1 }
  println(it.show())
  println(it.debug())
}
"#,
    );
    assert!(
        stdout.contains("default-show") && stdout.contains("item-debug"),
        "expected both default and impl-provided methods to print; stdout={stdout}"
    );
}

/// 4. Missing a non-default (abstract) method must still produce a
///    "missing method" error. Default methods don't shield abstract
///    siblings from that requirement.
#[test]
fn test_missing_abstract_method_still_errors() {
    let errs = type_errors(
        r#"
trait Display2 {
  fn show(self) -> String { "default-show" }
  fn debug(self) -> String
}

type Item { v: Int }

trait Display2 for Item {}

fn main() {}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("missing method") && e.contains("debug")),
        "expected missing-method error for the abstract `debug`, got: {errs:?}"
    );
    // And specifically NOT for `show` (which has a default body).
    assert!(
        !errs
            .iter()
            .any(|e| e.contains("missing method") && e.contains("'show'")),
        "did not expect a missing-method error for the defaulted `show`, got: {errs:?}"
    );
}

/// 5. A default method body can call other trait methods on `self` —
///    including abstract ones the impl is required to provide. Dispatch
///    must route the abstract call to the impl's version even though
///    the calling site is in the default body (which lives on the trait,
///    not on the impl).
#[test]
fn test_default_method_calls_other_trait_method_on_self() {
    let errs = type_errors(
        r#"
trait Describable {
  fn name(self) -> String
  fn greet(self) -> String { "hi, " + self.name() }
}

type Person { who: String }

trait Describable for Person {
  fn name(self) -> String { self.who }
}

fn main() {
  let p = Person { who: "alice" }
  println(p.greet())
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected default body calling other trait method on self to typecheck, got: {errs:?}"
    );

    let stdout = run_silt(
        "default_calls_other",
        r#"
trait Describable {
  fn name(self) -> String
  fn greet(self) -> String { "hi, " + self.name() }
}

type Person { who: String }

trait Describable for Person {
  fn name(self) -> String { self.who }
}

fn main() {
  let p = Person { who: "alice" }
  println(p.greet())
}
"#,
    );
    assert!(
        stdout.contains("hi, alice"),
        "expected default to dispatch through impl-provided abstract method; stdout={stdout}"
    );
}

/// 6. Default methods on a parameterized impl target. The default body
///    is shared across instantiations and must work when the impl is
///    `trait X for Box(a)`.
#[test]
fn test_default_method_on_parameterized_impl_target() {
    let errs = type_errors(
        r#"
type Box(a) { value: a }

trait Wrapper {
  fn describe(self) -> String { "a wrapped value" }
}

trait Wrapper for Box(a) {}

fn main() {
  let b = Box { value: 42 }
  println(b.describe())
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected default method on parameterized impl target to typecheck, got: {errs:?}"
    );

    let stdout = run_silt(
        "default_parameterized",
        r#"
type Box(a) { value: a }

trait Wrapper {
  fn describe(self) -> String { "a wrapped value" }
}

trait Wrapper for Box(a) {}

fn main() {
  let b = Box { value: 42 }
  println(b.describe())
}
"#,
    );
    assert!(
        stdout.contains("a wrapped value"),
        "expected default body to run on parameterized impl; stdout={stdout}"
    );
}

/// 7. Formatter roundtrip — a trait with a defaulted method formats
///    and reparses to an AST with the same default body.
#[test]
fn test_formatter_roundtrip_preserves_default_body() {
    let src = "trait X {\n  fn f(self) -> Int { 42 }\n}\n";
    let formatted = silt::formatter::format(src).expect("format");

    // The formatted output must still reparse and the trait method must
    // carry a non-Unit body (the default) — i.e. is_signature_only is
    // false on the parsed-back FnDecl.
    let tokens = silt::lexer::Lexer::new(&formatted).tokenize().expect("lex");
    let prog = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
    let trait_decl = prog
        .decls
        .iter()
        .find_map(|d| match d {
            silt::ast::Decl::Trait(t) => Some(t),
            _ => None,
        })
        .expect("expected a trait decl in reparsed program");
    assert_eq!(trait_decl.methods.len(), 1, "expected 1 method");
    let m = &trait_decl.methods[0];
    assert!(
        !m.is_signature_only,
        "expected method with default body to NOT be marked signature-only after roundtrip; \
         formatted source was:\n{formatted}"
    );

    // Idempotency: formatting twice yields the same result.
    let formatted2 = silt::formatter::format(&formatted).expect("format2");
    assert_eq!(
        formatted, formatted2,
        "format is not idempotent for default methods"
    );
}

/// 7b. Formatter roundtrip for an abstract (signature-only) trait method:
///     the body of the reparsed FnDecl must remain marked signature-only
///     (so the default-method machinery doesn't accidentally treat it
///     as a defaulted method that returns unit).
#[test]
fn test_formatter_roundtrip_preserves_abstract_method() {
    let src = "trait X {\n  fn f(self) -> Int\n}\n";
    let formatted = silt::formatter::format(src).expect("format");

    // The formatted output must NOT introduce a body for the abstract
    // method (no `= ()` and no `{ }`).
    assert!(
        !formatted.contains("= ()"),
        "abstract method must not be formatted with `= ()`; got:\n{formatted}"
    );

    let tokens = silt::lexer::Lexer::new(&formatted).tokenize().expect("lex");
    let prog = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
    let trait_decl = prog
        .decls
        .iter()
        .find_map(|d| match d {
            silt::ast::Decl::Trait(t) => Some(t),
            _ => None,
        })
        .expect("expected a trait decl in reparsed program");
    assert_eq!(trait_decl.methods.len(), 1, "expected 1 method");
    let m = &trait_decl.methods[0];
    assert!(
        m.is_signature_only,
        "expected abstract method to remain signature-only after roundtrip; \
         formatted source was:\n{formatted}"
    );

    let formatted2 = silt::formatter::format(&formatted).expect("format2");
    assert_eq!(
        formatted, formatted2,
        "format is not idempotent for abstract methods"
    );
}

/// 8. Default method that calls a supertrait method — interaction with
///    Agent 2's supertrait feature. Inside `Sub`'s default body we call
///    a method from its supertrait `Sup` on `self`.
#[test]
fn test_default_method_calls_supertrait_method() {
    let errs = type_errors(
        r#"
trait Sup {
  fn sup_method(self) -> String
}

trait Sub: Sup {
  fn sub_method(self) -> String { "sub-of-" + self.sup_method() }
}

type Thing { v: Int }

trait Sup for Thing {
  fn sup_method(self) -> String { "concrete" }
}

trait Sub for Thing {}

fn main() {
  let t = Thing { v: 1 }
  println(t.sub_method())
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected default body that calls supertrait method on self to typecheck, got: {errs:?}"
    );

    let stdout = run_silt(
        "default_calls_supertrait",
        r#"
trait Sup {
  fn sup_method(self) -> String
}

trait Sub: Sup {
  fn sub_method(self) -> String { "sub-of-" + self.sup_method() }
}

type Thing { v: Int }

trait Sup for Thing {
  fn sup_method(self) -> String { "concrete" }
}

trait Sub for Thing {}

fn main() {
  let t = Thing { v: 1 }
  println(t.sub_method())
}
"#,
    );
    assert!(
        stdout.contains("sub-of-concrete"),
        "expected default body to call through to supertrait impl; stdout={stdout}"
    );
}
