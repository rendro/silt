//! Locks that raw opcode names do not leak into user-facing `VmError`
//! messages.
//!
//! Background: round-58 fixed one site where the VM emitted
//! `"frame underflow in invoke_callable"` — the bare `invoke_callable`
//! identifier is a Rust method name, not anything a silt user could
//! meaningfully interpret. Several internal-invariant sites in
//! `src/vm/execute.rs` leaked similar raw opcode names (`SetLocal`,
//! `MakeClosure`, `MakeTuple`, `MakeList`, `MakeMap`, `MakeSet`).
//!
//! These invariant paths are not reachable from valid typed silt, so the
//! tests hand-build corrupt bytecode (the pattern in
//! `tests/lang/round80_vm_dispatch_bounds_tests.rs`) and assert on the
//! message the VM actually returns: the canonical `internal VM error:`
//! phrasing, with no opcode name in it.

use std::sync::Arc;

use silt::Value;
use silt::Vm;
use silt::bytecode::{Chunk, Function, Op};
use silt::source::Span;

fn span() -> Span {
    Span::BUILTIN
}

fn make_function(build: impl FnOnce(&mut Chunk)) -> Arc<Function> {
    let mut func = Function::new("<leak-test>".to_string(), 0);
    build(&mut func.chunk);
    Arc::new(func)
}

/// Run `script`, expect a `VmError`, and check that its message carries
/// `phrase` and does not name the opcode `op_name`.
fn assert_clean_error(script: Arc<Function>, phrase: &str, op_name: &str) {
    let mut vm = Vm::new();
    let err = vm
        .run(script)
        .expect_err("corrupt bytecode must surface as a VmError");
    let msg = format!("{err}");
    assert!(
        msg.contains(phrase),
        "expected the user-facing phrase {phrase:?}; got: {msg}"
    );
    assert!(
        !msg.contains(op_name),
        "the raw opcode name `{op_name}` leaked into the error: {msg}"
    );
}

#[test]
fn set_local_out_of_range_names_local_binding() {
    let script = make_function(|chunk| {
        let one = chunk.add_constant(Value::Int(1)).unwrap();
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(one, span());
        chunk.emit_op(Op::SetLocal, span());
        chunk.emit_u16(100, span()); // slot far past the stack
        chunk.emit_op(Op::Return, span());
    });
    assert_clean_error(
        script,
        "internal VM error: local binding slot out of range",
        "SetLocal",
    );
}

#[test]
fn make_closure_on_non_closure_names_closure_construction() {
    let script = make_function(|chunk| {
        let not_a_closure = chunk.add_constant(Value::Int(7)).unwrap();
        chunk.emit_op(Op::MakeClosure, span());
        chunk.emit_u16(not_a_closure, span());
        chunk.emit_u8(0, span()); // no upvalues
        chunk.emit_op(Op::Return, span());
    });
    assert_clean_error(
        script,
        "internal VM error: closure construction constant is not a closure",
        "MakeClosure",
    );
}

#[test]
fn make_tuple_over_count_names_tuple_construction() {
    let script = make_function(|chunk| {
        chunk.emit_op(Op::MakeTuple, span());
        chunk.emit_u8(5, span()); // empty stack
        chunk.emit_op(Op::Return, span());
    });
    assert_clean_error(
        script,
        "internal VM error: tuple construction count 5",
        "MakeTuple",
    );
}

#[test]
fn make_list_over_count_names_list_construction() {
    let script = make_function(|chunk| {
        chunk.emit_op(Op::MakeList, span());
        chunk.emit_u16(5, span());
        chunk.emit_op(Op::Return, span());
    });
    assert_clean_error(
        script,
        "internal VM error: list construction count 5",
        "MakeList",
    );
}

#[test]
fn make_map_over_count_names_map_construction() {
    let script = make_function(|chunk| {
        chunk.emit_op(Op::MakeMap, span());
        chunk.emit_u16(3, span()); // three pairs, empty stack
        chunk.emit_op(Op::Return, span());
    });
    assert_clean_error(
        script,
        "internal VM error: map construction needs 6 values",
        "MakeMap",
    );
}

#[test]
fn make_set_over_count_names_set_construction() {
    let script = make_function(|chunk| {
        chunk.emit_op(Op::MakeSet, span());
        chunk.emit_u16(5, span());
        chunk.emit_op(Op::Return, span());
    });
    assert_clean_error(
        script,
        "internal VM error: set construction count 5",
        "MakeSet",
    );
}
