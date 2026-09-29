//! Round 74 — `collections.rs` adopts the canonical
//! `"<fn> requires <Kind>, got <kind>"` wording.
//!
//! Round 73f converted `numeric.rs` and `string.rs` to the canonical
//! type-mismatch shape `"<fn> requires <Kind>, got <kind>"`.
//! `collections.rs` was missed by that sweep — five terse "requires
//! int" / "index must be int" sites lacked the `, got <kind>` suffix:
//!
//!   - `list.get`     index-must-be-int (was: "list.get index must be int")
//!   - `list.set`     index-must-be-int (was: "list.set index must be int")
//!   - `list.take`    requires-int      (was: "list.take requires int")
//!   - `list.drop`    requires-int      (was: "list.drop requires int")
//!   - `list.remove_at` index-must-be-int (was: "list.remove_at index must be int")
//!
//! Lock the canonical shape with behavioral tests, one per site. The
//! runtime path is reachable by passing a non-Int second argument that the typechecker can't catch (a polymorphic `Int|String`
//! union via a generic param doesn't fit the pre-fix grammar — instead
//! we use string-typed positional args which fail at runtime).

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;
use std::sync::Arc;

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

// ── BEHAVIORAL locks: runtime emits canonical wording ───────────────

#[test]
fn list_get_non_int_index_runtime_says_requires_int_got_kind() {
    // Pass a String where Int is required. `run_err` ignores typechecker
    // diagnostics (the call discards the `check(&mut program)` result)
    // and forwards the AST to the compiler/VM — so the runtime guard
    // `Value::Int(n) = &args[1] else { ... }` in `collections.rs::get`
    // fires and produces the canonical `<fn> requires Int, got <kind>`
    // wording, exercised by this test.
    //
    // Round 75 lock tightening: pin the full canonical substring (incl.
    // `, got`) and the offending kind. The previous OR-arm
    // `err.contains("Int")` passed vacuously — any error mentioning
    // "Int" satisfied it (including the typechecker's
    // "type mismatch: expected Int, got String"), nullifying the
    // behavioral check.
    let err = run_err(
        r#"
import list
fn main() { list.get([1, 2, 3], "x") }
"#,
    );
    assert!(
        err.contains("list.get requires Int, got"),
        "expected list.get runtime to emit the canonical \
         `list.get requires Int, got <kind>` wording; got: {err}"
    );
    assert!(
        err.contains("String"),
        "expected the offending kind `String` to appear in the \
         canonical wording; got: {err}"
    );
}

#[test]
fn list_take_non_int_count_runtime_says_requires_int_got_kind() {
    let err = run_err(
        r#"
import list
fn main() { list.take([1, 2, 3], "x") }
"#,
    );
    assert!(
        err.contains("list.take requires Int, got"),
        "expected list.take runtime to emit the canonical \
         `list.take requires Int, got <kind>` wording; got: {err}"
    );
    assert!(
        err.contains("String"),
        "expected the offending kind `String` to appear in the \
         canonical wording; got: {err}"
    );
}

#[test]
fn list_drop_non_int_count_runtime_says_requires_int_got_kind() {
    let err = run_err(
        r#"
import list
fn main() { list.drop([1, 2, 3], "x") }
"#,
    );
    assert!(
        err.contains("list.drop requires Int, got"),
        "expected list.drop runtime to emit the canonical \
         `list.drop requires Int, got <kind>` wording; got: {err}"
    );
    assert!(
        err.contains("String"),
        "expected the offending kind `String` to appear in the \
         canonical wording; got: {err}"
    );
}

#[test]
fn list_set_non_int_index_runtime_says_requires_int_got_kind() {
    let err = run_err(
        r#"
import list
fn main() { list.set([1, 2, 3], "x", 9) }
"#,
    );
    assert!(
        err.contains("list.set requires Int, got"),
        "expected list.set runtime to emit the canonical \
         `list.set requires Int, got <kind>` wording; got: {err}"
    );
    assert!(
        err.contains("String"),
        "expected the offending kind `String` to appear in the \
         canonical wording; got: {err}"
    );
}

#[test]
fn list_remove_at_non_int_index_runtime_says_requires_int_got_kind() {
    let err = run_err(
        r#"
import list
fn main() { list.remove_at([1, 2, 3], "x") }
"#,
    );
    assert!(
        err.contains("list.remove_at requires Int, got"),
        "expected list.remove_at runtime to emit the canonical \
         `list.remove_at requires Int, got <kind>` wording; got: {err}"
    );
    assert!(
        err.contains("String"),
        "expected the offending kind `String` to appear in the \
         canonical wording; got: {err}"
    );
}
