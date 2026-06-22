//! Round 95: string interpolation of a non-Display value must be rejected,
//! at runtime for the polymorphic case as it already is at compile time
//! for the concrete case.
//!
//! ## Background
//!
//! silt's string interpolation requires the interpolated value's type to
//! implement `Display` (docs/language/loops-and-pipes.md: "interpolating
//! [a non-Display type] is a compile error"). For a *concrete* operand the
//! typechecker rejects the non-Display types (`Fn`, `Channel`) at compile
//! time.
//!
//! But silt infers trait bounds from body usage and does NOT statically
//! enforce them on polymorphic functions — exactly as
//! `fn pick(x: a, y: a) -> a { match x > y { ... } }` compiles with no
//! `where a: Compare` and errors at *runtime* on two Channels
//! ("cannot compare Channel and Channel"). Pre-fix, interpolation was the
//! lone inconsistency: `fn show(x: a) -> String { "{x}" }` compiled and,
//! when called with a function or channel, SILENTLY rendered `<fn:..>` /
//! `<channel:0>` instead of erroring — a silent-wrong-behavior hole.
//!
//! The fix makes `Op::DisplayValue` (emitted per interpolated segment)
//! error at the execution site for the same set the Display gate rejects,
//! mirroring how Compare is enforced at runtime for polymorphic code.
//!
//! ## Parity lock
//!
//! The runtime-rejected set (function-shaped values, `Channel`) is exactly
//! the *concrete* set the typechecker's interpolation Display gate rejects,
//! and the Display-able set (Int/String/List/record) is exactly what it
//! accepts. Both halves are asserted here so the runtime and compile-time
//! layers cannot drift apart.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

// ── Runtime harness (typecheck is intentionally non-fatal, matching the
//    other runtime regression suites: the polymorphic programs typecheck
//    clean and the behavior under test is at the VM execution site). The
//    program-under-test returns a String from `main` so we assert on the
//    rendered value directly rather than capturing stdout. ──────────────

fn run_ok(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    match vm.run(script).expect("expected success") {
        Value::String(s) => s,
        other => panic!("expected main to return a String, got {other:?}"),
    }
}

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect_err("expected runtime error").message
}

/// Collect typechecker diagnostics that mention the interpolation Display
/// gate, for the concrete-operand parity half.
fn display_typecheck_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    silt::typechecker::check(&mut program)
        .into_iter()
        .map(|e| e.message)
        .filter(|m| m.contains("does not implement Display"))
        .collect()
}

// ── Runtime half: the polymorphic bypass now errors at the exec site ────

#[test]
fn polymorphic_interp_of_fn_errors_at_runtime() {
    let src = r#"
fn show(x: a) -> String { "val={x}" }
fn helper() -> Int { 5 }
fn main() { println(show(helper)) }
"#;
    let err = run_err(src);
    assert!(
        err.contains("does not implement Display") && err.contains("'Fn'"),
        "expected a Display runtime error naming Fn, got: {err}"
    );
}

#[test]
fn polymorphic_interp_of_channel_errors_at_runtime() {
    let src = r#"
import channel
fn show(x: a) -> String { "val={x}" }
fn main() {
  let c = channel.new()
  println(show(c))
}
"#;
    let err = run_err(src);
    assert!(
        err.contains("does not implement Display") && err.contains("'Channel'"),
        "expected a Display runtime error naming Channel, got: {err}"
    );
}

// ── Runtime half: Display-able values still render correctly ────────────

#[test]
fn polymorphic_interp_of_display_values_still_works() {
    let src = r#"
type P { name: String, age: Int }
fn show(x: a) -> String { "val={x}" }
fn main() -> String {
  show(42) + "|" + show("hi") + "|" + show([1, 2, 3]) + "|" + show(P { name: "x", age: 3 })
}
"#;
    let out = run_ok(src);
    assert!(out.contains("val=42"), "Int interp: {out}");
    assert!(out.contains("val=hi"), "String interp: {out}");
    assert!(out.contains("val=[1, 2, 3]"), "List interp: {out}");
    assert!(out.contains("val=P {"), "record interp: {out}");
}

// ── Parity half: the runtime-rejected set matches the concrete
//    compile-time-rejected set, and the accepted set matches too ─────────

#[test]
fn concrete_interp_of_fn_is_a_compile_error() {
    // Same `Fn` operand that errors at runtime through the polymorphic
    // bypass is rejected at compile time when its type is concrete.
    let src = r#"
fn helper() -> Int { 5 }
fn main() { println("val={helper}") }
"#;
    let errs = display_typecheck_errors(src);
    assert!(
        !errs.is_empty(),
        "concrete Fn interpolation must be a compile-time Display error"
    );
}

#[test]
fn concrete_interp_of_display_types_typechecks_clean() {
    // The accepted set (Int/String/List) must NOT trip the Display gate —
    // proving the runtime fix did not over-reject.
    let src = r#"
fn main() -> String {
  let n = 42
  let s = "hi"
  let xs = [1, 2, 3]
  "a={n} b={s} c={xs}"
}
"#;
    assert!(
        display_typecheck_errors(src).is_empty(),
        "Int/String/List interpolation must typecheck without a Display error"
    );
    // And it runs, producing the rendered values.
    let out = run_ok(src);
    assert!(out.contains("a=42 b=hi c=[1, 2, 3]"), "got: {out}");
}
