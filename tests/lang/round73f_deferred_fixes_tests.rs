//! Round-73 follow-up, fix #3: builtins that were handed the wrong kind
//! of value used terse diagnostics (`"int.abs requires an int"`) that hid
//! the offending kind. They now go through `super::common::require_int` /
//! `require_string`, which produce `"<fn> requires <Kind>, got <kind>"`.
//!
//! The typechecker rejects the program first, so this runtime wording is
//! only reachable by running the VM with the typechecker's verdict
//! discarded. The other round-73f locks (occurs-check wording, Display ==
//! message() for stdlib errors) are golden cases
//! (`tests/golden/lang/*/round73f_deferred_fixes__*`).

use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;

#[test]
fn fix3_runtime_emits_canonical_kind_named_form() {
    // Behavioral lock: passing the wrong kind to a converted builtin
    // produces the canonical "requires <Kind>, got <kind>" form.
    //
    // Round 75 lock tightening: the previous shape used the `silt run`
    // binary, which runs the full pipeline (typechecker → compiler →
    // VM). The typechecker rejects `int.abs("not-an-int")` with
    // "type mismatch: expected Int, got String" before the runtime
    // ever fires, so the original assertion (absence of pre-fix
    // wording) passed vacuously even if the runtime had silently
    // regressed. We restructure to drive parser → compiler → VM
    // directly while ignoring typechecker diagnostics — exactly the
    // pattern round 74's `collections_canonical_kind_wording_tests.rs`
    // uses to genuinely exercise the runtime guard. A positive
    // assertion on `int.abs requires Int, got` then locks the
    // canonical shape (and its presence catches a regression).
    let src = r#"
import int
fn main() { int.abs("not-an-int") }
"#;
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    // Discard typechecker diagnostics; the runtime is what we want to
    // exercise. The compiler still consumes the AST and produces
    // bytecode that the VM runs.
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler
        .compile_program(&program)
        .expect("expected the compiler to accept the program when typechecker errors are dropped");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm
        .run(script)
        .expect_err("expected a runtime error from int.abs(\"not-an-int\")");
    let err_str = format!("{err}");
    // Positive lock: canonical `<fn> requires <Kind>, got <kind>` form.
    assert!(
        err_str.contains("int.abs requires Int, got"),
        "expected int.abs runtime to emit the canonical \
         `int.abs requires Int, got <kind>` wording; got: {err_str}"
    );
    assert!(
        err_str.contains("String"),
        "expected the offending kind `String` in the canonical wording; got: {err_str}"
    );
    // Negative locks: pre-fix terse wording must not return.
    assert!(
        !err_str.contains("int.abs requires an int") && !err_str.contains("int.abs requires a int"),
        "terse pre-fix wording must not surface anywhere. Got: {err_str}"
    );
}
