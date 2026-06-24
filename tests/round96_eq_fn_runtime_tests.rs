//! Round 96: polymorphic `==` / `!=` on function-shaped values must error
//! at runtime, mirroring the concrete-operand compile error.
//!
//! ## Background
//!
//! silt infers trait bounds from body usage and does NOT statically enforce
//! them on polymorphic templates: `pending_numeric_checks`
//! (src/typechecker/inference.rs) skips operands whose type is still a
//! `Var`, on the documented promise that "the VM catches it at runtime with
//! a clean operator-domain diagnostic." That promise was honored for
//! ordering (`compare()`'s catch-all in src/vm/arithmetic.rs errors on
//! function-shaped values) and for string-interpolation Display (the
//! `Op::DisplayValue` gate), but NOT for equality.
//!
//! Pre-fix, a polymorphic `fn eq(x: a, y: a) -> Bool { x == y }` called
//! with two distinct functions silently returned `false`: `Op::Eq` only
//! ran `check_same_type` (which checks the value discriminant) and then
//! `language_eq`, whose catch-all delegates to `PartialEq for Value`, which
//! does `Arc::ptr_eq` on closures and name-equality on builtins
//! (src/value.rs). The concrete form `f == g` is a compile error
//! (`is_valid_compare_operand` rejects `Type::Fun` for equality via its
//! `_ => false` arm), so the runtime path was the laundering bypass.
//!
//! The fix gates `Op::Eq` / `Op::Neq` against function-shaped operands and
//! errors with a Display-style message ("type 'Fn' does not implement
//! Equal"), matching how ordering and interpolation already fail at the
//! execution site.
//!
//! ## NOT rejected
//!
//! Channel / Handle / TcpListener / TcpStream are intentionally equatable
//! by identity at runtime and the typechecker accepts them
//! (`is_valid_compare_operand` accepts `Type::Channel` and every
//! `Type::Generic(..)`). The gate must leave those alone — covered below by
//! `polymorphic_eq_of_channels_still_returns_bool`.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

// ── Runtime harness: typecheck is intentionally non-fatal (the polymorphic
//    program typechecks clean and the behavior under test is at the VM
//    execution site, matching tests/round95_interp_display_runtime_tests.rs).
//    The program-under-test returns from `main`; we run it and inspect the
//    VM result / error directly. ──────────────────────────────────────────

fn run(input: &str) -> Result<Value, String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).map_err(|e| e.message)
}

/// Collect typechecker diagnostics that mention the equality operator
/// domain gate, for the concrete-operand parity half.
fn eq_typecheck_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    silt::typechecker::check(&mut program)
        .into_iter()
        .map(|e| e.message)
        .filter(|m| m.contains("requires a comparable type") || m.contains("does not implement"))
        .collect()
}

// ── Runtime half: the polymorphic bypass now errors at the exec site ──────

#[test]
fn polymorphic_eq_of_fns_errors_at_runtime() {
    // The finding's exact repro. Pre-fix this printed `false` and exited 0.
    let src = r#"
fn eq(x: a, y: a) -> Bool { x == y }
fn f() { 1 }
fn g() { 2 }
fn main() { println(eq(f, g)) }
"#;
    let err = run(src).expect_err("expected a runtime Equal error, got a clean result");
    assert!(
        err.contains("does not implement Equal") && err.contains("'Fn'"),
        "expected a runtime Equal error naming Fn, got: {err}"
    );
}

#[test]
fn polymorphic_neq_of_fns_errors_at_runtime() {
    // `!=` shares the bypass and must be gated identically.
    let src = r#"
fn neq(x: a, y: a) -> Bool { x != y }
fn f() { 1 }
fn main() { println(neq(f, f)) }
"#;
    let err = run(src).expect_err("expected a runtime Equal error, got a clean result");
    assert!(
        err.contains("does not implement Equal") && err.contains("'Fn'"),
        "expected a runtime Equal error naming Fn for !=, got: {err}"
    );
}

// ── Parity half: the concrete form is a compile error ─────────────────────

#[test]
fn concrete_eq_of_fns_is_a_compile_error() {
    let src = r#"
fn f() { 1 }
fn g() { 2 }
fn main() { println(f == g) }
"#;
    let errs = eq_typecheck_errors(src);
    assert!(
        !errs.is_empty(),
        "concrete `f == g` must be a compile-time error (got none)"
    );
}

// ── Non-regression: equatable-by-identity values must NOT be gated ────────

#[test]
fn polymorphic_eq_of_channels_still_returns_bool() {
    // Channels are equatable by identity at runtime and accepted by the
    // typechecker. The gate must leave them alone — comparing a channel to
    // itself yields `true`, not a runtime error.
    let src = r#"
import channel
fn eq(x: a, y: a) -> Bool { x == y }
fn main() -> Bool {
  let c = channel.new()
  eq(c, c)
}
"#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b, "a channel must equal itself"),
        Ok(other) => panic!("expected Bool, got {other:?}"),
        Err(e) => panic!("channel equality must not error, got: {e}"),
    }
}

#[test]
fn polymorphic_eq_of_ints_still_works() {
    // The accepted set is untouched: ordinary equality still computes.
    let src = r#"
fn eq(x: a, y: a) -> Bool { x == y }
fn main() -> Bool { eq(3, 3) }
"#;
    match run(src) {
        Ok(Value::Bool(b)) => assert!(b, "3 == 3 must be true"),
        Ok(other) => panic!("expected Bool, got {other:?}"),
        Err(e) => panic!("int equality must not error, got: {e}"),
    }
}
