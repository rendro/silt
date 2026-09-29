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
//! The discriminants of `Op::And` / `Op::Or`, so a variant insertion that
//! reorders them (and corrupts serialized bytecode) fails. The
//! short-circuit behaviour is locked by the golden cases
//! `tests/golden/lang/operators/round75_op_and_or_unreachable__*`.

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
