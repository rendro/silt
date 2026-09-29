//! Regression tests for the fn_body_types collision bug where trait impl
//! methods keyed under the bare method name (e.g., "show") would overwrite
//! a standalone function of the same name. The fix (inference.rs line 292)
//! keys entries under `lookup_name` — qualified for trait impls, bare for
//! standalone fns — so the two never collide.
//!
//! Both tests were designed to FAIL before the fix and PASS after.

use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;

// ── Helpers ─────────────────────────────────────────────────────────

/// Typecheck-only: collect error messages.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Full pipeline: lex -> parse -> typecheck -> compile -> run.
/// Panics on any error. Returns the top-level `main` return value.
fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let errs = typechecker::check(&mut program);
    let fatal: Vec<_> = errs
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(fatal.is_empty(), "type errors: {fatal:?}");
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

// ── Tests ───────────────────────────────────────────────────────────

/// Standalone function `show` with a different signature from the trait
/// method `show` must typecheck and run correctly. Before the fix, the
/// trait impl for `Showable.show` (returning String) overwrote the
/// standalone `show` (returning Int) in `fn_body_types`, causing a
/// spurious type mismatch error during scheme narrowing.
#[test]
fn test_standalone_fn_not_clobbered_by_trait_impl_method() {
    let src = r#"
trait Showable { fn show(self) -> String }
trait Showable for Int { fn show(self) -> String = "int" }
fn show(x) = x + 1
fn main() -> Int { show(41) }
"#;
    // Must have zero type errors
    let errs = type_errors(src);
    assert!(errs.is_empty(), "expected no type errors, got: {errs:?}");

    // Must produce the correct runtime value
    let v = run(src);
    match v {
        Value::Int(n) => assert_eq!(n, 42),
        other => panic!("expected Int(42), got {other:?}"),
    }
}

/// The trait method itself must still work correctly after the fix.
/// Calling `(5).show()` should dispatch to the `Showable for Int` impl
/// and return "int".
#[test]
fn test_trait_impl_method_still_works_independently() {
    let src = r#"
trait Showable { fn show(self) -> String }
trait Showable for Int { fn show(self) -> String = "int" }
fn show(x) = x + 1
fn main() -> String { (5).show() }
"#;
    // Must have zero type errors
    let errs = type_errors(src);
    assert!(errs.is_empty(), "expected no type errors, got: {errs:?}");

    // Must produce the correct runtime value
    let v = run(src);
    match v {
        Value::String(s) => assert_eq!(s.as_str(), "int"),
        other => panic!("expected String(\"int\"), got {other:?}"),
    }
}
