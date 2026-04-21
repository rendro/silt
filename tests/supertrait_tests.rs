//! Supertrait bounds tests (`trait Ordered: Equal { ... }`).
//!
//! Covers:
//! 1. Implementing a subtrait without the supertrait → error.
//! 2. `where a: Ordered` enables Equal methods on `a`.
//! 3. Transitive supertraits (C: B, B: A — implementing C requires A, B, C).
//! 4. Multiple supertraits (X: A + B).
//! 5. Unknown supertrait name → error.
//! 6. Formatter roundtrip preserves supertraits in source order.
//! 7. Runtime correctness: actual call through supertrait constraint
//!    resolves to the right impl.
//! 8. Cycle handling: `trait A: B { } trait B: A { }` does not infinite-
//!    loop the typechecker (behaviour under cycles is unspecified beyond
//!    "doesn't crash").

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

/// 1. Implementing a subtrait on a type that does not implement the
///    supertrait should error. Both supertrait and subtrait are user-defined
///    (so neither is auto-derived) and the impl only provides the subtrait.
#[test]
fn test_subtrait_impl_without_supertrait_errors() {
    let errs = type_errors(
        r#"
trait Eq2 {
  fn eq2(self, other: Self) -> Bool
}

trait Ord2: Eq2 {
  fn lt2(self, other: Self) -> Bool
}

type Foo { v: Int }

trait Ord2 for Foo {
  fn lt2(self, other: Foo) -> Bool { self.v < other.v }
}

fn main() {}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("implements 'Ord2'") && e.contains("supertrait 'Eq2'")),
        "expected supertrait-missing error for Foo's Ord2 impl, got: {errs:?}"
    );
}

/// 1b. Implementing both subtrait and supertrait should typecheck cleanly.
#[test]
fn test_subtrait_impl_with_supertrait_ok() {
    let errs = type_errors(
        r#"
trait Eq2 {
  fn eq2(self, other: Self) -> Bool
}

trait Ord2: Eq2 {
  fn lt2(self, other: Self) -> Bool
}

type Foo { v: Int }

trait Eq2 for Foo {
  fn eq2(self, other: Foo) -> Bool { self.v == other.v }
}

trait Ord2 for Foo {
  fn lt2(self, other: Foo) -> Bool { self.v < other.v }
}

fn main() {}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected no errors when both supertrait and subtrait are impl'd, got: {errs:?}"
    );
}

/// 2. Inside a `where a: Ord2` body, methods from Eq2 (the supertrait)
///    must be callable on `a`.
#[test]
fn test_where_subtrait_enables_supertrait_methods() {
    let errs = type_errors(
        r#"
trait Eq2 {
  fn eq2(self, other: Self) -> Bool
}

trait Ord2: Eq2 {
  fn lt2(self, other: Self) -> Bool
}

type Foo { v: Int }

trait Eq2 for Foo {
  fn eq2(self, other: Foo) -> Bool { self.v == other.v }
}

trait Ord2 for Foo {
  fn lt2(self, other: Foo) -> Bool { self.v < other.v }
}

-- This should typecheck because Eq2 is a supertrait of Ord2.
fn cmp(a: t, b: t) -> Bool where t: Ord2 {
  match a.eq2(b) {
    true -> false
    false -> a.lt2(b)
  }
}

fn main() {
  let x = Foo { v: 1 }
  let y = Foo { v: 2 }
  println(cmp(x, y))
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected supertrait-method call to typecheck under subtrait constraint, got: {errs:?}"
    );
}

/// 3. Transitive supertraits: `trait C: B`, `trait B: A`. Implementing C
///    on a type without A must error (and without B too).
#[test]
fn test_transitive_supertraits_require_all_impls() {
    let errs = type_errors(
        r#"
trait A1 {
  fn a1(self) -> Int
}

trait B1: A1 {
  fn b1(self) -> Int
}

trait C1: B1 {
  fn c1(self) -> Int
}

type Bar { v: Int }

-- Only impl C1, missing A1 and B1.
trait C1 for Bar {
  fn c1(self) -> Int { self.v }
}

fn main() {}
"#,
    );
    let has_b = errs
        .iter()
        .any(|e| e.contains("implements 'C1'") && e.contains("supertrait 'B1'"));
    assert!(
        has_b,
        "expected missing-B1 supertrait error for Bar's C1 impl, got: {errs:?}"
    );
}

/// 3b. Transitive supertrait constraint expansion: `where t: C1` should
///     enable A1 methods (transitively).
#[test]
fn test_transitive_supertrait_method_calls_through_where() {
    let errs = type_errors(
        r#"
trait A1 {
  fn a1(self) -> Int
}

trait B1: A1 {
  fn b1(self) -> Int
}

trait C1: B1 {
  fn c1(self) -> Int
}

type Bar { v: Int }

trait A1 for Bar {
  fn a1(self) -> Int { self.v }
}

trait B1 for Bar {
  fn b1(self) -> Int { self.v + 1 }
}

trait C1 for Bar {
  fn c1(self) -> Int { self.v + 2 }
}

-- All three method calls must resolve via the C1 -> B1 -> A1 chain.
fn use_all(x: t) -> Int where t: C1 {
  x.a1() + x.b1() + x.c1()
}

fn main() {
  println(use_all(Bar { v: 10 }))
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected transitive-supertrait method call resolution to typecheck, got: {errs:?}"
    );
}

/// 4. Multiple supertraits via `+`. Implementing the subtrait without
///    either supertrait must error; with both, it succeeds.
#[test]
fn test_multiple_supertraits_require_all() {
    let errs = type_errors(
        r#"
trait Tr1 {
  fn t1(self) -> Int
}
trait Tr2 {
  fn t2(self) -> Int
}
trait Both: Tr1 + Tr2 {
  fn both(self) -> Int
}

type Baz { v: Int }

-- Only Tr1 impl, missing Tr2.
trait Tr1 for Baz {
  fn t1(self) -> Int { self.v }
}

trait Both for Baz {
  fn both(self) -> Int { self.v }
}

fn main() {}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("implements 'Both'") && e.contains("supertrait 'Tr2'")),
        "expected missing-Tr2 supertrait error, got: {errs:?}"
    );
}

#[test]
fn test_multiple_supertraits_all_satisfied() {
    let errs = type_errors(
        r#"
trait Tr1 {
  fn t1(self) -> Int
}
trait Tr2 {
  fn t2(self) -> Int
}
trait Both: Tr1 + Tr2 {
  fn both(self) -> Int
}

type Baz { v: Int }

trait Tr1 for Baz {
  fn t1(self) -> Int { self.v }
}
trait Tr2 for Baz {
  fn t2(self) -> Int { self.v + 1 }
}
trait Both for Baz {
  fn both(self) -> Int { self.t1() + self.t2() }
}

fn main() {
  let b = Baz { v: 5 }
  println(b.both())
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected all-supertrait-satisfied to typecheck, got: {errs:?}"
    );
}

/// 5. Unknown supertrait name → error at trait declaration.
#[test]
fn test_unknown_supertrait_errors() {
    let errs = type_errors(
        r#"
trait Foo: NotATrait {
  fn foo(self) -> Int
}

fn main() {}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("trait 'Foo'") && e.contains("unknown supertrait 'NotATrait'")),
        "expected unknown-supertrait error, got: {errs:?}"
    );
}

/// 6. Formatter roundtrip: a trait with supertraits formats and reparses
///    to the same AST shape (specifically: the supertraits list survives).
#[test]
fn test_formatter_roundtrip_preserves_supertraits() {
    let src = "trait X: A + B {\n  fn x(self) -> Int\n}\n";
    let formatted = silt::formatter::format(src).expect("format");

    // Reparse the formatted output and verify the supertraits list survives.
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
    let supertrait_names: Vec<String> = trait_decl
        .supertraits
        .iter()
        .map(|(name, _args)| name.to_string())
        .collect();
    assert_eq!(
        supertrait_names,
        vec!["A".to_string(), "B".to_string()],
        "supertraits did not roundtrip; formatted source was:\n{formatted}"
    );

    // Also: idempotency — formatting twice yields the same result.
    let formatted2 = silt::formatter::format(&formatted).expect("format2");
    assert_eq!(
        formatted, formatted2,
        "format is not idempotent for supertraits"
    );
}

/// 6b. Formatter renders a single supertrait without `+`.
#[test]
fn test_formatter_single_supertrait() {
    let src = "trait Sub: Sup {\n  fn s(self) -> Int\n}\n";
    let formatted = silt::formatter::format(src).expect("format");
    assert!(
        formatted.contains("trait Sub: Sup"),
        "expected single supertrait to format as `trait Sub: Sup`, got:\n{formatted}"
    );
}

/// 7. Runtime correctness — calling a supertrait method via a subtrait
///    constraint dispatches to the right impl.
#[test]
fn test_runtime_dispatch_through_supertrait_constraint() {
    use std::process::Command;
    let src = r#"
trait Eq2 {
  fn eq2(self, other: Self) -> Bool
}

trait Ord2: Eq2 {
  fn lt2(self, other: Self) -> Bool
}

type Foo { v: Int }

trait Eq2 for Foo {
  fn eq2(self, other: Foo) -> Bool { self.v == other.v }
}

trait Ord2 for Foo {
  fn lt2(self, other: Foo) -> Bool { self.v < other.v }
}

fn label(a: t, b: t) -> String where t: Ord2 {
  match a.eq2(b) {
    true -> "equal"
    false -> match a.lt2(b) {
      true -> "less"
      false -> "greater"
    }
  }
}

fn main() {
  println(label(Foo { v: 1 }, Foo { v: 2 }))
  println(label(Foo { v: 3 }, Foo { v: 3 }))
  println(label(Foo { v: 5 }, Foo { v: 4 }))
}
"#;
    // Unique temp-file name — pid + atomic counter + nanosecond timestamp —
    // so parallel `cargo test` invocations / re-runs never race on the
    // same path. Same pattern as `concurrency_stress_property_tests.rs`.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tmp = std::env::temp_dir().join(format!(
        "silt_supertrait_runtime_{}_{}_{}.silt",
        std::process::id(),
        ts,
        n
    ));
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
    // Lock the full output sequence. `main` prints label(1,2), label(3,3),
    // label(5,4) in that order, so stdout must be exactly:
    //   less
    //   equal
    //   greater
    // The previous `contains(..) && contains(..) && contains(..)` chain
    // didn't pin ordering — a bugged compile-order swap or a repeated
    // label would still satisfy it.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec!["less", "equal", "greater"],
        "expected exact label sequence, got stdout={stdout:?}"
    );
}

/// 8. Cycle handling: a self-loop or mutual loop in supertraits must not
///    hang the typechecker. The behaviour under cycles is unspecified beyond
///    "doesn't infinite-loop / crash" — this test merely asserts the
///    process completes within a reasonable time.
#[test]
fn test_supertrait_cycle_does_not_hang() {
    let _errs = type_errors(
        r#"
trait Cyc1: Cyc2 {
  fn c1(self) -> Int
}
trait Cyc2: Cyc1 {
  fn c2(self) -> Int
}

fn main() {}
"#,
    );
    // No assertion on exact errors — just that we got here without
    // infinite-looping. The expand_with_supertraits `seen` set is the
    // safety net, and so is the validate_trait_impls iteration shape
    // (single pass over `self.traits`).
}

/// 8b. Self-cycle: a trait listing itself as a supertrait. Must also
///     terminate.
#[test]
fn test_supertrait_self_cycle_does_not_hang() {
    let _errs = type_errors(
        r#"
trait SelfCyc: SelfCyc {
  fn s(self) -> Int
}

fn main() {}
"#,
    );
}
