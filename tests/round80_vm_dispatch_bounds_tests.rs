//! Round-80 VM dispatch-bounds defense-in-depth lock tests.
//!
//! Two findings, both unreachable from the legitimate compiler today
//! but trivially reachable from corrupt bytecode (e.g. a future
//! refactor that mis-emits `argc`, or a fuzz harness that exercises
//! the dispatch loop with hand-built chunks). Without the gate, the
//! VM panics with a Rust `index out of bounds` instead of returning a
//! `VmError` — which violates the project-wide invariant that every
//! VM-internal invariant breach surfaces as `internal VM error: ...`.
//!
//! ## L6 — `Op::CallMethod` argc==0 sanity gate
//!
//! `Op::CallMethod` reads a u8 `argc`, computes
//! `receiver_slot = stack.len() - argc`, then indexes
//! `self.stack[receiver_slot]`. The pre-fix gate only checked the
//! upper bound (`argc > stack.len()`), so `argc == 0` produced
//! `receiver_slot == stack.len()` and the very next access OOB-
//! panicked. The compiler always emits `argc = (args.len() + 1) as u8`
//! at `src/compiler/mod.rs:2250` so it's not user-reachable, but the
//! gate is cheap defense-in-depth that locks the invariant.
//!
//! ## L7 — `Op::PopN` saturating-vs-strict underflow
//!
//! `Op::PopN`'s pre-fix body used
//! `self.stack.len().saturating_sub(count)`, silently truncating to
//! an empty stack on over-pop. Every other dispatch arm errors loudly
//! on stack underflow (cf. `Op::Pop` → `self.pop()?`). A compiler bug
//! that emitted too-large a popcount would therefore corrupt
//! subsequent execution rather than fail fast. The fix replaces the
//! saturating subtraction with a strict bounds check that returns
//! `VmError::new("internal VM error: PopN underflow ...")`.
//!
//! ## Why integration tests, not unit tests
//!
//! `src/vm/tests.rs` already exposes the raw-bytecode-injection
//! pattern (`Function::new(...)` + `chunk.emit_op(...)` + `Vm::run`).
//! Rather than add to the unit module, we replicate the same shape
//! here against the public API (`silt::bytecode::*`, `silt::Vm`,
//! `silt::Value`) so the lock survives any future privacy tightening
//! of the unit module.

use std::sync::Arc;

use silt::Value;
use silt::Vm;
use silt::bytecode::{Chunk, Function, Op};
use silt::lexer::Span;

fn span() -> Span {
    Span::new(0, 0)
}

/// Helper mirroring `src/vm/tests.rs::make_function`: build a
/// `Function` from raw bytecode construction.
fn make_function(build: impl FnOnce(&mut Chunk)) -> Arc<Function> {
    let mut func = Function::new("<round80-test>".to_string(), 0);
    build(&mut func.chunk);
    Arc::new(func)
}

// ── L6: Op::CallMethod argc==0 gate ──────────────────────────────────

/// Hand-build a chunk that runs `Op::CallMethod` with `argc = 0`.
/// Pre-fix this would panic with `index out of bounds` because
/// `receiver_slot = stack.len() - 0 = stack.len()` and the very next
/// `self.stack[receiver_slot].clone()` reads past the end.
/// Post-fix it must surface as a `VmError` carrying the canonical
/// `internal VM error:` prefix.
#[test]
fn l6_callmethod_argc_zero_returns_internal_vm_error() {
    let script = make_function(|chunk| {
        let method_idx = chunk
            .add_constant(Value::String("foo".to_string()))
            .unwrap();
        // No receiver pushed — empty stack.
        chunk.emit_op(Op::CallMethod, span());
        chunk.emit_u16(method_idx, span());
        chunk.emit_u8(0, span()); // argc = 0 (corrupt — receiver missing)
        chunk.emit_op(Op::Return, span());
    });

    let mut vm = Vm::new();
    let result = vm.run(script);
    let err = result.expect_err(
        "Op::CallMethod with argc=0 must surface as VmError, not Rust \
         panic — round-80 L6 defense-in-depth gate",
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("internal VM error:"),
        "round-80 L6: argc==0 error must use the canonical \
         `internal VM error:` prefix; got: {msg}"
    );
    assert!(
        msg.contains("argc"),
        "round-80 L6: error message should mention `argc` so the failure \
         points at the offending bytecode field; got: {msg}"
    );
}

/// Sibling check for the upper-bound gate: `argc` larger than the
/// stack must also produce a `VmError` (this path was already gated
/// pre-fix; we lock it in alongside the new lower-bound gate so a
/// future edit can't accidentally relax both).
#[test]
fn l6_callmethod_argc_exceeds_stack_returns_vm_error() {
    let script = make_function(|chunk| {
        let method_idx = chunk
            .add_constant(Value::String("foo".to_string()))
            .unwrap();
        // Push a single Int as receiver, then claim argc=5 — only one
        // value on the stack, so 5 > 1 must trip the gate.
        let one = chunk.add_constant(Value::Int(1)).unwrap();
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(one, span());
        chunk.emit_op(Op::CallMethod, span());
        chunk.emit_u16(method_idx, span());
        chunk.emit_u8(5, span()); // argc=5, stack has 1
        chunk.emit_op(Op::Return, span());
    });

    let mut vm = Vm::new();
    let result = vm.run(script);
    let err = result.expect_err(
        "Op::CallMethod with argc > stack.len() must surface as VmError",
    );
    let msg = format!("{err}");
    // Either the original "exceeds stack size" wording or the new
    // unified `internal VM error:` wording is acceptable here — the
    // post-fix path collapses both bounds into the same gate.
    assert!(
        msg.contains("internal VM error:") || msg.contains("exceeds stack"),
        "round-80 L6 sibling: argc>stack error wording unexpected; got: {msg}"
    );
}

// ── L7: Op::PopN strict underflow ────────────────────────────────────

/// Hand-build a chunk that runs `Op::PopN` with a count larger than
/// the stack height. Pre-fix this silently truncated to an empty
/// stack via `saturating_sub`, masking any compiler bug that emitted
/// too-large a popcount. Post-fix it must surface as a `VmError`
/// with the canonical `internal VM error: PopN underflow` wording.
#[test]
fn l7_popn_overflow_returns_internal_vm_error() {
    let script = make_function(|chunk| {
        // Push two Ints, then PopN 7 — stack has 2, count is 7,
        // saturating_sub would silently leave us with an empty stack.
        let a = chunk.add_constant(Value::Int(1)).unwrap();
        let b = chunk.add_constant(Value::Int(2)).unwrap();
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::PopN, span());
        chunk.emit_u8(7, span()); // count=7, stack has 2
        chunk.emit_op(Op::Unit, span());
        chunk.emit_op(Op::Return, span());
    });

    let mut vm = Vm::new();
    let result = vm.run(script);
    let err = result.expect_err(
        "Op::PopN with count > stack.len() must surface as VmError, not \
         silently truncate — round-80 L7 defense-in-depth gate",
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("internal VM error:"),
        "round-80 L7: PopN underflow error must use the canonical \
         `internal VM error:` prefix; got: {msg}"
    );
    assert!(
        msg.contains("PopN underflow"),
        "round-80 L7: error message should mention `PopN underflow` so \
         the failure pinpoints the dispatch arm; got: {msg}"
    );
}

/// Positive control: `Op::PopN` with a legitimate count must still
/// work — we pop exactly the number of elements pushed and observe
/// the empty stack via a follow-up `Op::Unit`. This guards against an
/// over-eager "always error" regression.
#[test]
fn l7_popn_legitimate_count_still_works() {
    let script = make_function(|chunk| {
        let a = chunk.add_constant(Value::Int(1)).unwrap();
        let b = chunk.add_constant(Value::Int(2)).unwrap();
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(a, span());
        chunk.emit_op(Op::Constant, span());
        chunk.emit_u16(b, span());
        chunk.emit_op(Op::PopN, span());
        chunk.emit_u8(2, span()); // count=2, stack has 2 — exactly empties it
        chunk.emit_op(Op::Unit, span());
        chunk.emit_op(Op::Return, span());
    });

    let mut vm = Vm::new();
    let result = vm.run(script).expect(
        "Op::PopN with count == stack.len() must succeed — guards \
         against an over-eager `>=` regression of the strict-underflow \
         check",
    );
    assert_eq!(result, Value::Unit);
}

// ── Source-grep belt-and-suspenders locks ────────────────────────────

const VM_EXECUTE_RS: &str = include_str!("../src/vm/execute.rs");

/// Source-level lock: the L6 gate string must remain in execute.rs.
/// If a future refactor reverts it back to the upper-bound-only check,
/// this test fires immediately even before the unit-style test above
/// runs the bytecode.
#[test]
fn l6_callmethod_gate_present_in_execute_rs() {
    assert!(
        VM_EXECUTE_RS.contains("argc == 0 || argc > self.stack.len()"),
        "round-80 L6: `Op::CallMethod` must reject both argc==0 (receiver \
         required) and argc>stack.len() in a single combined gate. The \
         exact gate string `argc == 0 || argc > self.stack.len()` was not \
         found in src/vm/execute.rs — has the defense-in-depth check \
         regressed?"
    );
}

/// Source-level lock: the L7 strict bounds check must remain in
/// execute.rs and the `saturating_sub` form must NOT come back.
#[test]
fn l7_popn_strict_underflow_check_present_in_execute_rs() {
    assert!(
        VM_EXECUTE_RS.contains("internal VM error: PopN underflow"),
        "round-80 L7: `Op::PopN` must surface over-pop as \
         `internal VM error: PopN underflow ...`. That literal was not \
         found in src/vm/execute.rs — has the strict bounds check been \
         relaxed back to `saturating_sub`?"
    );

    // The PopN dispatch arm specifically must no longer use
    // `saturating_sub`. We can't grep the whole file (other arms
    // legitimately use saturating arithmetic), but we can at least
    // assert the exact pre-fix line is gone.
    assert!(
        !VM_EXECUTE_RS.contains("self.stack.len().saturating_sub(count)"),
        "round-80 L7: the pre-fix `self.stack.len().saturating_sub(count)` \
         expression in `Op::PopN` masks compiler bugs that emit too-large \
         a popcount. Replace it with a strict bounds check that returns \
         `VmError::new(\"internal VM error: PopN underflow ...\")`."
    );
}
