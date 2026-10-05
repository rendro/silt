//! The bytecode verifier.
//!
//! [`verify`] checks the code of one function by abstract
//! interpretation of the height of its frame, with the effects of the
//! instruction table ([`ops`](super::ops)). Code that passes cannot make
//! the VM take a value off an empty frame, read a slot, a constant or an
//! upvalue that is not there, find a constant of the wrong
//! kind, or continue at a place where no instruction starts; so the VM
//! does not look for any of that. What the verifier does not know is
//! what kind of value an instruction finds: that is the typechecker's
//! claim, and the VM's `type_confusion` errors are for when it is wrong.
//!
//! The rules:
//!
//! - The code is instructions from its first byte to its last, and the
//!   last one does not go on to a next one.
//! - An instruction takes no more values off the frame than the frame
//!   holds, and a `cut` is no higher than what is left.
//! - A slot operand is below the height; an upvalue operand is below the
//!   function's upvalue count; a jump goes to the start of an
//!   instruction.
//! - A constant operand is in the pool and of the kind the table says.
//!   A function constant that captures values is only the operand of a
//!   `MakeClosure` that captures as many, each from a slot below the
//!   height or from an upvalue the function has.
//! - `CallMethod` has a receiver.
//! - The height is the same on every path into an instruction, with one
//!   exception. Where paths arrive with different heights, the frame is
//!   known to hold the smallest of them, and only these may follow until
//!   the height is one number again: `GetLocal` and `Constant` (pushes
//!   that need nothing above the known values), `Jump`, `Return` and
//!   `Panic` (which leave), and `Slide`, which cuts the frame to a
//!   height of its own and so ends the exception. This is the shape of
//!   the code the compiler emits where a pattern test fails with the
//!   sub-values it was looking at still in the frame, and where the arms
//!   of a `match` end with their bindings still under their results.

use std::fmt;

use crate::value::Value;

use super::ops::{ConstKind, Flow, Instr, Operand, decode};
use super::{Const, Function};

/// Why a function's code is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    /// The offset of the instruction that is wrong.
    pub at: usize,
    pub what: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at offset {}: {}", self.at, self.what)
    }
}

/// What is known about the frame where an instruction starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
    /// The number of values the frame holds on every path here.
    height: usize,
    /// Whether some path arrives with more.
    ragged: bool,
}

/// Check the code of `function` (not of the functions among its
/// constants, which were checked when they were made).
pub fn verify(function: &Function) -> Result<(), VerifyError> {
    let code = function.chunk().code();
    let fail = |at: usize, what: String| Err(VerifyError { at, what });

    // The instructions, and which one starts at each offset.
    const NONE: usize = usize::MAX;
    let mut instrs = Vec::new();
    let mut starting_at = vec![NONE; code.len()];
    let mut at = 0;
    while at < code.len() {
        let Some((instr, next)) = decode(code, at) else {
            return fail(at, "no instruction is encoded here".into());
        };
        starting_at[at] = instrs.len();
        instrs.push((at, instr, next));
        at = next;
    }
    match instrs.last() {
        None => return fail(0, "the function has no code".into()),
        Some((at, instr, _)) if matches!(instr.effect().flow, Flow::Next | Flow::Branch) => {
            return fail(*at, "the code runs off its end".into());
        }
        Some(_) => {}
    }

    let mut frames: Vec<Option<Frame>> = vec![None; instrs.len()];
    frames[0] = Some(Frame {
        height: function.arity(),
        ragged: false,
    });
    let mut work = vec![0];
    while let Some(index) = work.pop() {
        let (at, instr, next) = instrs[index];
        let frame = frames[index].expect("an instruction in the work list has a frame");
        let after = check(function, at, &instr, frame)?;
        let mut arrive = |offset: usize| -> Result<(), VerifyError> {
            let Some(&target) = starting_at.get(offset).filter(|index| **index != NONE) else {
                return fail(
                    at,
                    format!("control goes to {offset}, where no instruction starts"),
                );
            };
            let merged = match frames[target] {
                None => after,
                Some(known) => Frame {
                    height: known.height.min(after.height),
                    ragged: known.ragged || after.ragged || known.height != after.height,
                },
            };
            if frames[target] != Some(merged) {
                frames[target] = Some(merged);
                work.push(target);
            }
            Ok(())
        };
        let target = || {
            let mut target = None;
            instr.operands(|operand| {
                if let Operand::Target(offset) = operand {
                    target = Some(offset);
                }
            });
            target.expect("an instruction that jumps has a target")
        };
        match instr.effect().flow {
            Flow::Next => arrive(next)?,
            Flow::Branch => {
                arrive(next)?;
                arrive(target())?;
            }
            Flow::Jump => arrive(target())?,
            Flow::End => {}
        }
    }
    Ok(())
}

/// Check the instruction `instr` at `at`, which starts with `frame`,
/// and give the frame it leaves.
fn check(
    function: &Function,
    at: usize,
    instr: &Instr,
    frame: Frame,
) -> Result<Frame, VerifyError> {
    let chunk = function.chunk();
    let op = instr.op().name();
    let fail = |what: String| Err(VerifyError { at, what });

    if frame.ragged
        && !matches!(
            instr,
            Instr::GetLocal { .. }
                | Instr::Constant { .. }
                | Instr::Slide { .. }
                | Instr::Jump { .. }
                | Instr::Return
                | Instr::Panic
        )
    {
        return fail(format!(
            "`{op}` runs where the paths into it leave different heights"
        ));
    }

    let constant = |k: Const, kind: ConstKind| -> Result<(), String> {
        let Some(value) = chunk.constants().get(k.index()) else {
            return Err(format!(
                "`{op}` names constant {} of {}",
                k.index(),
                chunk.constants().len()
            ));
        };
        let fits = match kind {
            // A function that captures values is made by `MakeClosure`
            // only.
            ConstKind::Any => match value {
                Value::VmClosure(closure) => {
                    closure.function.upvalue_count() == closure.upvalues.len()
                }
                _ => true,
            },
            ConstKind::Str => matches!(value, Value::String(_)),
            ConstKind::Tag => matches!(value, Value::VariantConstructor(_)),
            ConstKind::Type => matches!(value, Value::TypeDescriptor(_)),
            ConstKind::Func => matches!(value, Value::VmClosure(_)),
            ConstKind::Int => matches!(value, Value::Int(_)),
            ConstKind::Float => matches!(value, Value::Float(_)),
        };
        match fits {
            true => Ok(()),
            false => Err(format!(
                "`{op}` names constant {} as {kind:?}, and it is {value:?}",
                k.index()
            )),
        }
    };

    let mut wrong: Option<String> = None;
    instr.operands(|operand| {
        let result = match operand {
            Operand::Plain | Operand::Target(_) => Ok(()),
            Operand::Slot(slot) if slot >= frame.height => Err(format!(
                "`{op}` names slot {slot} of a frame of {}",
                frame.height
            )),
            Operand::Slot(_) => Ok(()),
            Operand::Upvalue(index) if index >= function.upvalue_count() => Err(format!(
                "`{op}` names upvalue {index} of {}",
                function.upvalue_count()
            )),
            Operand::Upvalue(_) => Ok(()),
            Operand::Const(k, kind) => constant(k, kind),
            Operand::Strs(names) => names
                .iter(chunk.code())
                .try_for_each(|k| constant(k, ConstKind::Str)),
            Operand::Captures(captures) => captures.iter(chunk.code()).try_for_each(|capture| {
                let index = usize::from(capture.index);
                match capture.is_local {
                    true if index >= frame.height => Err(format!(
                        "`{op}` captures slot {index} of a frame of {}",
                        frame.height
                    )),
                    false if index >= function.upvalue_count() => Err(format!(
                        "`{op}` captures upvalue {index} of {}",
                        function.upvalue_count()
                    )),
                    _ => Ok(()),
                }
            }),
        };
        if wrong.is_none() {
            wrong = result.err();
        }
    });
    if let Some(what) = wrong {
        return fail(what);
    }

    match *instr {
        Instr::MakeClosure { f, captures }
            if chunk.closure(f).function.upvalue_count() != captures.len() =>
        {
            return fail(format!(
                "`{op}` captures {} values for a function that has {} upvalues",
                captures.len(),
                chunk.closure(f).function.upvalue_count()
            ));
        }
        Instr::CallMethod { argc: 0, .. } => {
            return fail(format!("`{op}` has no receiver"));
        }
        _ => {}
    }

    let effect = instr.effect();
    if effect.pops > frame.height {
        return fail(format!(
            "`{op}` takes {} values off a frame of {}",
            effect.pops, frame.height
        ));
    }
    let popped = frame.height - effect.pops;
    if let Some(cut) = effect.cut
        && cut > popped
    {
        return fail(format!(
            "`{op}` cuts a frame of {popped} values back to {cut}"
        ));
    }
    Ok(Frame {
        height: effect.cut.unwrap_or(popped) + effect.pushes,
        ragged: frame.ragged && effect.cut.is_none(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::bytecode::{Asm, Emitter, NO_TRAIT, Op, VmClosure};
    use crate::source::Span;

    // Hand-built functions: the bytes are written here, not by the
    // emitter, which would refuse most of them itself.

    fn op(op: Op) -> u8 {
        op as u8
    }

    /// Why the verifier rejects the code `code` of a function with no
    /// parameters, no upvalues and the constants `constants`.
    fn rejected(code: Vec<u8>, constants: Vec<Value>) -> String {
        rejected_fn(Function::unverified(0, 0, code, constants))
    }

    fn rejected_fn(function: Function) -> String {
        verify(&function)
            .expect_err("the verifier must reject this function")
            .to_string()
    }

    /// A verified function with one upvalue, as a constant.
    fn capturing_function() -> Value {
        let mut e = Emitter::new("inner".into(), 0);
        e.emit(Asm::GetUpvalue { index: 0 }, Span::BUILTIN).unwrap();
        e.emit(Asm::Return, Span::BUILTIN).unwrap();
        Value::VmClosure(Arc::new(VmClosure {
            function: Arc::new(e.finish(1).unwrap()),
            upvalues: vec![],
        }))
    }

    #[test]
    fn bad_01_no_code() {
        assert_eq!(
            rejected(vec![], vec![]),
            "at offset 0: the function has no code"
        );
    }

    #[test]
    fn bad_02_unknown_opcode() {
        let why = rejected(vec![op(Op::Unit), 250, op(Op::Return)], vec![]);
        assert_eq!(why, "at offset 1: no instruction is encoded here");
    }

    #[test]
    fn bad_03_operand_cut_off_by_the_end_of_the_code() {
        let why = rejected(
            vec![op(Op::Unit), op(Op::Return), op(Op::Constant), 0],
            vec![],
        );
        assert_eq!(why, "at offset 2: no instruction is encoded here");
    }

    #[test]
    fn bad_04_code_runs_off_its_end() {
        let why = rejected(vec![op(Op::Unit), op(Op::Pop)], vec![]);
        assert_eq!(why, "at offset 1: the code runs off its end");
    }

    #[test]
    fn bad_05_stack_underflow() {
        let why = rejected(vec![op(Op::Unit), op(Op::Add), op(Op::Return)], vec![]);
        assert_eq!(why, "at offset 1: `Add` takes 2 values off a frame of 1");
        // A count operand above the frame is the same defect.
        let why = rejected(vec![op(Op::MakeTuple), 5, op(Op::Return)], vec![]);
        assert_eq!(
            why,
            "at offset 0: `MakeTuple` takes 5 values off a frame of 0"
        );
        let why = rejected(vec![op(Op::MakeMap), 3, 0, op(Op::Return)], vec![]);
        assert_eq!(
            why,
            "at offset 0: `MakeMap` takes 6 values off a frame of 0"
        );
        let why = rejected(vec![op(Op::Unit), op(Op::Call), 1, op(Op::Return)], vec![]);
        assert_eq!(why, "at offset 1: `Call` takes 2 values off a frame of 1");
    }

    #[test]
    fn bad_06_slot_out_of_the_frame() {
        let code = vec![op(Op::Unit), op(Op::SetLocal), 100, 0, op(Op::Return)];
        assert_eq!(
            rejected(code, vec![]),
            "at offset 1: `SetLocal` names slot 100 of a frame of 1"
        );
        let code = vec![op(Op::GetLocal), 0, 0, op(Op::Return)];
        assert_eq!(
            rejected(code, vec![]),
            "at offset 0: `GetLocal` names slot 0 of a frame of 0"
        );
    }

    #[test]
    fn bad_07_constant_out_of_the_pool() {
        let code = vec![op(Op::Constant), 3, 0, op(Op::Return)];
        assert_eq!(
            rejected(code, vec![Value::Int(1)]),
            "at offset 0: `Constant` names constant 3 of 1"
        );
    }

    #[test]
    fn bad_08_constant_of_the_wrong_kind() {
        // A field name that is no string.
        let code = vec![op(Op::Unit), op(Op::GetField), 0, 0, op(Op::Return)];
        let why = rejected(code, vec![Value::Int(7)]);
        assert_eq!(
            why,
            "at offset 1: `GetField` names constant 0 as Str, and it is 7"
        );
        // A closure made of something that is no function.
        let code = vec![op(Op::MakeClosure), 0, 0, 0, op(Op::Return)];
        let why = rejected(code, vec![Value::Int(42)]);
        assert_eq!(
            why,
            "at offset 0: `MakeClosure` names constant 0 as Func, and it is 42"
        );
        // A variant test against something that is no variant.
        let code = vec![op(Op::Unit), op(Op::TestTag), 0, 0, op(Op::Return)];
        let why = rejected(code, vec![Value::String("Some".into())]);
        assert!(why.contains("`TestTag` names constant 0 as Tag"), "{why}");
        // A field name in a list that is no string.
        let code = vec![op(Op::Unit), op(Op::RecordUpdate), 1, 0, 0, op(Op::Return)];
        let why = rejected(code, vec![Value::Int(7)]);
        assert!(
            why.contains("`RecordUpdate` names constant 0 as Str"),
            "{why}"
        );
    }

    #[test]
    fn bad_09_jump_into_the_middle_of_an_instruction() {
        // 0000 Jump -> 0004, which is the operand of the Constant at 0003.
        let code = vec![op(Op::Jump), 1, 0, op(Op::Constant), 0, 0, op(Op::Return)];
        assert_eq!(
            rejected(code, vec![Value::Int(1)]),
            "at offset 0: control goes to 4, where no instruction starts"
        );
        // And past the end of the code.
        let code = vec![op(Op::Jump), 9, 0, op(Op::Unit), op(Op::Return)];
        assert_eq!(
            rejected(code, vec![]),
            "at offset 0: control goes to 12, where no instruction starts"
        );
    }

    #[test]
    fn bad_10_paths_with_different_heights_into_an_ordinary_instruction() {
        // 0000 True; 0001 JumpIfFalse -> 0006; 0004 Unit; 0005 Unit;
        // 0006 Unit: reached with 0 values (the jump) and with 2.
        let code = vec![
            op(Op::True),
            op(Op::JumpIfFalse),
            2,
            0,
            op(Op::Unit),
            op(Op::Unit),
            op(Op::Unit),
            op(Op::Return),
        ];
        assert_eq!(
            rejected(code, vec![]),
            "at offset 6: `Unit` runs where the paths into it leave different heights"
        );
    }

    #[test]
    fn paths_with_different_heights_into_a_slide_are_accepted() {
        // The same join, followed by what the compiler emits there: a
        // push and a `Slide` that cuts the frame to one height.
        // 0000 Unit; 0001 True; 0002 JumpIfFalse -> 0007; 0005 Unit;
        // 0006 Unit; 0007 GetLocal 0; 0010 Slide 0; 0013 Return.
        let code = vec![
            op(Op::Unit),
            op(Op::True),
            op(Op::JumpIfFalse),
            2,
            0,
            op(Op::Unit),
            op(Op::Unit),
            op(Op::GetLocal),
            0,
            0,
            op(Op::Slide),
            0,
            0,
            op(Op::Return),
        ];
        verify(&Function::unverified(0, 0, code, vec![])).unwrap();
    }

    #[test]
    fn bad_11_upvalue_the_function_does_not_have() {
        let code = vec![op(Op::GetUpvalue), 0, op(Op::Return)];
        assert_eq!(
            rejected(code.clone(), vec![]),
            "at offset 0: `GetUpvalue` names upvalue 0 of 0"
        );
        verify(&Function::unverified(0, 1, code, vec![])).unwrap();
    }

    #[test]
    fn bad_12_closure_made_with_the_wrong_captures() {
        // No captures for a function with one upvalue.
        let code = vec![op(Op::MakeClosure), 0, 0, 0, op(Op::Return)];
        assert_eq!(
            rejected(code, vec![capturing_function()]),
            "at offset 0: `MakeClosure` captures 0 values for a function that has 1 upvalues"
        );
        // A capture of a slot the frame does not have.
        let code = vec![op(Op::MakeClosure), 0, 0, 1, 1, 3, op(Op::Return)];
        assert_eq!(
            rejected(code, vec![capturing_function()]),
            "at offset 0: `MakeClosure` captures slot 3 of a frame of 0"
        );
        // A capture of an upvalue the function does not have.
        let code = vec![op(Op::MakeClosure), 0, 0, 1, 0, 0, op(Op::Return)];
        assert_eq!(
            rejected(code, vec![capturing_function()]),
            "at offset 0: `MakeClosure` captures upvalue 0 of 0"
        );
        // The function used as a plain constant, with nothing captured.
        let code = vec![op(Op::Constant), 0, 0, op(Op::Return)];
        let why = rejected(code, vec![capturing_function()]);
        assert!(why.contains("`Constant` names constant 0 as Any"), "{why}");
    }

    #[test]
    fn bad_13_method_call_without_a_receiver() {
        let [lo, hi] = NO_TRAIT.to_le_bytes();
        let code = vec![op(Op::CallMethod), 0, 0, 0, lo, hi, op(Op::Return)];
        assert_eq!(
            rejected(code, vec![Value::String("foo".into())]),
            "at offset 0: `CallMethod` has no receiver"
        );
        // And with more arguments than the frame holds.
        let code = vec![
            op(Op::Unit),
            op(Op::CallMethod),
            0,
            0,
            5,
            lo,
            hi,
            op(Op::Return),
        ];
        assert_eq!(
            rejected(code, vec![Value::String("foo".into())]),
            "at offset 1: `CallMethod` takes 5 values off a frame of 1"
        );
    }

    #[test]
    fn bad_14_frame_cut_back_to_more_than_it_holds() {
        let code = vec![op(Op::Unit), op(Op::Slide), 5, 0, op(Op::Return)];
        assert_eq!(
            rejected(code, vec![]),
            "at offset 1: `Slide` cuts a frame of 0 values back to 5"
        );
        // `Recur` of one binding into slot 3 of a frame of one value.
        let code = vec![op(Op::Unit), op(Op::Recur), 1, 3, 0, op(Op::Return)];
        assert_eq!(
            rejected(code, vec![]),
            "at offset 1: `Recur` cuts a frame of 0 values back to 3"
        );
    }

    #[test]
    fn bad_15_jump_back_before_the_start_of_the_code() {
        let code = vec![op(Op::Unit), op(Op::JumpBack), 9, 0];
        assert_eq!(
            rejected(code, vec![]),
            "at offset 1: no instruction is encoded here"
        );
    }

    #[test]
    fn unreachable_code_is_decoded_and_not_interpreted() {
        // After the `Return`, an `Add` nobody runs: fine. A byte that is
        // no instruction: not fine.
        let code = vec![op(Op::Unit), op(Op::Return), op(Op::Add), op(Op::Return)];
        verify(&Function::unverified(0, 0, code, vec![])).unwrap();
    }

    #[test]
    fn the_arguments_are_the_frame_a_function_starts_with() {
        let code = vec![op(Op::GetLocal), 1, 0, op(Op::Return)];
        verify(&Function::unverified(2, 0, code.clone(), vec![])).unwrap();
        assert_eq!(
            rejected_fn(Function::unverified(1, 0, code, vec![])),
            "at offset 0: `GetLocal` names slot 1 of a frame of 1"
        );
    }
}
