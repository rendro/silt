//! Round-75 VM-2 regression tests for `Op::And` / `Op::Or` dispatch
//! arms.
//!
//! ## What changed
//!
//! `src/vm/execute.rs:1282-1313` previously contained eager-eval
//! dispatch arms for `Op::And` and `Op::Or` that popped two booleans
//! and pushed `a && b` / `a || b`. The compiler does not emit those
//! opcodes — `compiler/mod.rs:2326, 2337` lower `BinOp::And` to
//! `JumpIfFalse` short-circuit and `BinOp::Or` to `JumpIfTrue`
//! short-circuit; the `BinOp::And | BinOp::Or => unreachable!()` at
//! `compiler/mod.rs:2358` confirms no other emission path exists.
//!
//! Round-75 VM-2 replaces the two arms with `unreachable!()` to match
//! the `Op::LoopSetup` precedent at `execute.rs:2070-2076` (deliberate
//! "crash loudly on accidental re-emission" pattern). The variants
//! stay in the `Op` enum so the bytecode discriminants do not shift
//! — bytecode is a serialization format and the discriminant
//! ordering is part of its on-disk shape.
//!
//! ## What this test locks
//!
//! 1. **Source-grep lock.** `src/compiler/mod.rs` contains no
//!    `chunk.write_op(Op::And` or `chunk.write_op(Op::Or` — i.e. no
//!    emission path exists for the opcodes.
//! 2. **Behavioral lock.** Short-circuit semantics still hold: the
//!    second operand's expression is never evaluated when the first
//!    determines the result. Compile and run a Silt program that
//!    would panic if eager-eval semantics ran, and assert the
//!    program returns the short-circuit value without panicking.
//! 3. **Discriminant lock.** Pin the exact `Op::And as u8` and
//!    `Op::Or as u8` values so a future variant insertion that
//!    silently reordered them — and thereby corrupted any pre-
//!    serialized bytecode — fails this test.

use silt::bytecode::Op;

/// Discriminant lock. The bytecode serialization contract treats
/// `Op as u8` as part of the disk format; reordering variants would
/// silently break previously-saved bytecode. The values below are
/// the Round-75 baseline read off `src/bytecode.rs`.
///
/// Op enum order (from src/bytecode.rs:31 onward):
///   Constant=0, Unit=1, True=2, False=3,
///   Add=4, Sub=5, Mul=6, Div=7, Mod=8,
///   Eq=9, Neq=10, Lt=11, Gt=12, Leq=13, Geq=14,
///   Negate=15, Not=16,
///   And=17, Or=18,
///   ...
#[test]
fn op_and_discriminant_is_pinned() {
    assert_eq!(
        Op::And as u8,
        17,
        "Op::And's discriminant is part of the bytecode serialization \
         format. If this assertion fails, a variant was inserted before \
         Op::And and any pre-Round-75 bytecode written to disk now \
         deserializes to the wrong opcode. Update this constant only \
         alongside an intentional bytecode-format bump."
    );
}

#[test]
fn op_or_discriminant_is_pinned() {
    assert_eq!(
        Op::Or as u8,
        18,
        "Op::Or's discriminant is part of the bytecode serialization \
         format. Same rationale as `op_and_discriminant_is_pinned`."
    );
}

/// Source-grep lock: the compiler must never emit Op::And or Op::Or.
/// The `BinOp::And | BinOp::Or => unreachable!()` at
/// `compiler/mod.rs:2358` already enforces this at the AST → bytecode
/// boundary, but a regression that introduces a new emission path
/// (e.g. a future "fold both operands eagerly" optimization) would
/// be silently wrong. This test scans the compiler source for
/// `chunk.write_op(Op::And` / `chunk.write_op(Op::Or` and refuses if
/// either appears.
#[test]
fn compiler_source_does_not_emit_op_and_or() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("compiler")
        .join("mod.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

    // We grep for the canonical emission helper used throughout the
    // compiler. Both `chunk.write_op(...)` and `chunk.emit_op(...)`
    // patterns appear; check both to be safe.
    for forbidden in [
        "write_op(Op::And",
        "write_op(Op::Or",
        "emit_op(Op::And",
        "emit_op(Op::Or",
    ] {
        assert!(
            !source.contains(forbidden),
            "compiler/mod.rs contains `{forbidden}` — round-75 VM-2 \
             requires that Op::And/Op::Or never be emitted. The \
             compiler must lower BinOp::And/Or to JumpIfFalse / \
             JumpIfTrue short-circuit form (see compiler/mod.rs:2326, \
             2337); the dispatch arms in execute.rs are now \
             `unreachable!()`."
        );
    }
}

/// Behavioral lock: `false and <panicking expr>` must short-circuit
/// to `false` without evaluating the right-hand side. If the compiler
/// regressed and emitted `Op::And` (eager-eval) instead of
/// `JumpIfFalse`, the right-hand panic would fire and the program
/// would error.
///
/// We use `panic` as the right-hand side rather than a separate
/// function because `panic` is a builtin that is guaranteed to abort
/// the running task. The script returns 0 on the success path; if
/// short-circuit semantics regressed, the panic surfaces as a
/// VM error.
#[test]
fn and_short_circuits_to_false_without_evaluating_rhs() {
    use silt::compiler::Compiler;
    use silt::lexer::Lexer;
    use silt::parser::Parser;
    use silt::value::Value;
    use silt::vm::Vm;
    use std::sync::Arc;

    let src = r#"
fn main() {
  -- If `&&` were eager-eval, panic("rhs ran") would fire here. The
  -- compiler emits JumpIfFalse, so the panic is unreached and the
  -- expression evaluates to `false`. We test the value via match
  -- so a regression that pushed Bool(true) instead of Bool(false)
  -- also fails.
  let r = false && panic("rhs ran")
  match r {
    true -> 1
    false -> 0
  }
}
"#;

    let tokens = Lexer::new(src).tokenize().expect("lex");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let result = vm.run(script).expect("run");
    assert_eq!(
        result,
        Value::Int(0),
        "`false and panic(...)` must short-circuit to false (returning \
         the false branch's 0); a regression that emitted Op::And and \
         eager-evaluated the rhs would have produced a panic VmError, \
         not Int(0)."
    );
}

/// Symmetric behavioral lock for `or`: `true or panic(...)` must
/// short-circuit to `true` without evaluating the right-hand side.
/// If the compiler regressed and emitted `Op::Or` (eager-eval), the
/// rhs panic would fire.
#[test]
fn or_short_circuits_to_true_without_evaluating_rhs() {
    use silt::compiler::Compiler;
    use silt::lexer::Lexer;
    use silt::parser::Parser;
    use silt::value::Value;
    use silt::vm::Vm;
    use std::sync::Arc;

    let src = r#"
fn main() {
  let r = true || panic("rhs ran")
  match r {
    true -> 1
    false -> 0
  }
}
"#;

    let tokens = Lexer::new(src).tokenize().expect("lex");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let result = vm.run(script).expect("run");
    assert_eq!(
        result,
        Value::Int(1),
        "`true or panic(...)` must short-circuit to true (returning \
         the true branch's 1); a regression that emitted Op::Or and \
         eager-evaluated the rhs would have produced a panic VmError, \
         not Int(1)."
    );
}
