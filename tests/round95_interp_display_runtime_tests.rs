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
//! The runtime-rejected set is exactly the *concrete* set the
//! typechecker's interpolation Display gate rejects — every first-class
//! value whose canonical type name is absent from the Display
//! `trait_impl_set`: function-shaped values (`Fn`), `Channel`, `Handle`
//! (task handles), `TcpListener` / `TcpStream` (opaque network resources
//! left explicitly unprintable, src/typechecker/mod.rs ~7878), and the
//! reflective descriptor values (`TypeDescriptor` / `PrimitiveDescriptor`).
//! The Display-able set (Int/String/List/record) is exactly what it
//! accepts. Both halves are asserted here so the runtime and compile-time
//! layers cannot drift apart.
//!
//! Round-95 follow-up: the original gate covered only `Fn` + `Channel`
//! and the prose falsely claimed those were "the only first-class values"
//! the Display gate rejects yet can flow through an unbounded type
//! variable. `Handle` (e.g. `task.spawn`) and the Tcp resources reproduce
//! the same silent-`<handle:0>` / `<tcp-stream:0>` rendering; the gate
//! now rejects the full set, sourced from the single VM-side oracle
//! `Vm::value_implements_display` so the runtime layer cannot drift.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::scheduler::test_support::InProcessRunner;
use silt::value::{TaskHandle, Value};
use silt::vm::Vm;
use std::sync::Arc;
use std::time::Duration;

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

// ── Round-95 follow-up: the gate was incomplete. Handle / TcpListener /
//    TcpStream are no-Display first-class types that ALSO flow through an
//    unbounded type variable and pre-fix rendered a debug string silently
//    (`<handle:0>` / `<tcp-listener:0>`) and exited 0. These exercise the
//    full scheduler runtime via InProcessRunner because `task.spawn` and
//    `tcp.listen` need the real runtime, not a bare `Vm::run`. ───────────

#[test]
fn polymorphic_interp_of_handle_errors_at_runtime() {
    // `task.spawn` yields a `Handle`. Interpolating it through the
    // polymorphic `show` must error at the execution site rather than
    // silently rendering `val=<handle:0>` and exiting 0 (the round-95
    // hole this follow-up closes — the original repro in the finding).
    let src = r#"
import task
fn show(x: a) -> String { "val={x}" }
fn main() {
  let h = task.spawn(fn() { 42 })
  println(show(h))
}
"#;
    let outcome = InProcessRunner::new(src)
        .with_budget(Duration::from_secs(10))
        .run_trial();
    let err = outcome
        .error_message
        .expect("expected a runtime Display error, got clean exit");
    assert!(
        err.contains("does not implement Display") && err.contains("'Handle'"),
        "expected a Display runtime error naming Handle, got: {err}"
    );
}

#[test]
fn polymorphic_interp_of_tcp_listener_errors_at_runtime() {
    // `tcp.listen` on an ephemeral local port yields a `TcpListener`
    // (an opaque resource left explicitly unprintable). Interpolating it
    // through the polymorphic `show` must error rather than silently
    // rendering `val=<tcp-listener:0>`.
    let src = r#"
import tcp
fn show(x: a) -> String { "val={x}" }
fn main() {
  match tcp.listen("127.0.0.1:0") {
    Ok(l) -> println(show(l))
    Err(e) -> println("listen failed")
  }
}
"#;
    let outcome = InProcessRunner::new(src)
        .with_budget(Duration::from_secs(10))
        .run_trial();
    let err = outcome
        .error_message
        .expect("expected a runtime Display error, got clean exit");
    assert!(
        err.contains("does not implement Display") && err.contains("'TcpListener'"),
        "expected a Display runtime error naming TcpListener, got: {err}"
    );
}

// ── Oracle lock: the VM-side `value_implements_display` predicate is the
//    single source of truth for the runtime gate. Assert it rejects EVERY
//    no-Display first-class value (so the gate and the predicate cannot
//    drift) and accepts the printable ones. This catches a future variant
//    being added to the rejected set in the typechecker but not here. ────

#[test]
fn value_implements_display_predicate_covers_every_no_display_value() {
    // Rejected: function-shaped, Channel, Handle, Tcp resources,
    // descriptors. (Tcp stream/listener constructed directly is awkward —
    // they hold live sockets — so the runtime tests above cover the live
    // path; here we cover the cheaply-constructible no-Display values.)
    let handle = Value::Handle(Arc::new(TaskHandle::new(0)));
    let type_desc = Value::TypeDescriptor("Int".to_string());
    let prim_desc = Value::PrimitiveDescriptor("Int".to_string());
    for v in [&handle, &type_desc, &prim_desc] {
        assert!(
            !Vm::value_implements_display(v),
            "no-Display value must be rejected by the predicate: {v:?}"
        );
    }
    // Accepted: the printable built-ins still pass.
    for v in [
        Value::Int(1),
        Value::String("x".into()),
        Value::Bool(true),
        Value::List(Arc::new(vec![Value::Int(1)])),
        Value::Unit,
    ] {
        assert!(
            Vm::value_implements_display(&v),
            "Display-able value must be accepted by the predicate: {v:?}"
        );
    }
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
