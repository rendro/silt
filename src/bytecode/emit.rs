//! The emitter: the one writer of bytecode.
//!
//! The compiler makes one [`Emitter`] per function it compiles and hands
//! it instructions; [`Emitter::finish`] gives the [`Function`]. This is
//! the whole emit API, and it is frozen for stage 7:
//!
//! | Call | What it does |
//! |---|---|
//! | [`Emitter::new`]`(name, arity, span)` | Start a function. Its frame holds its `arity` arguments, in slots `0..arity`. |
//! | [`Emitter::constant`]`(value, span)` | Put `value` in the function's constant pool (once, for the values that can be compared) and give the [`Const`] that names it. |
//! | [`Emitter::emit`]`(asm, span)` | Append the instruction [`Asm`], whose code is blamed on `span`. |
//! | [`Emitter::label`]`()` | A new [`Label`]: a place jumps can go to, not placed yet. |
//! | [`Emitter::bind`]`(label, span)` | Place `label` at the next instruction. |
//! | [`Emitter::bind_over`]`(label, over, span)` | The same, naming what the jumps to `label` go over, for the limit on a conditional jump's reach. |
//! | [`Emitter::height`]`()` | The number of values in the frame where the next instruction runs. |
//! | [`Emitter::reachable`]`()` | Whether control can reach the next instruction. |
//! | [`Emitter::ended`]`()` | Whether the code is complete: it ends in an instruction that ends the run, and nothing reaches past it. |
//! | [`Emitter::assume_height`]`(height)` | In unreachable code, what the height would be. |
//! | [`Emitter::finish`]`(upvalues)` | Verify the code and give the function, which captures `upvalues` values. |
//!
//! `if x { 1 } else { 2 }` for a function of one parameter `x`:
//!
//! ```
//! use silt::bytecode::{Asm, Emitter};
//! use silt::source::Span;
//! use silt::value::Value;
//!
//! let span = Span::BUILTIN;
//! let mut e = Emitter::new("pick".to_string(), 1, span)?;
//! let one = e.constant(Value::Int(1), span)?;
//! let two = e.constant(Value::Int(2), span)?;
//! let otherwise = e.label();
//! let end = e.label();
//! e.emit(Asm::GetLocal { slot: 0 }, span)?; // height 2
//! e.emit(Asm::JumpIfFalse { to: otherwise }, span)?; // height 1
//! e.emit(Asm::Constant { k: one }, span)?; // height 2
//! e.emit(Asm::Jump { to: end }, span)?; // the next instruction is unreachable
//! e.bind(otherwise, span)?; // reachable again, height 1
//! e.emit(Asm::Constant { k: two }, span)?;
//! e.bind(end, span)?;
//! assert_eq!(e.height(), 2);
//! e.emit(Asm::Return, span)?;
//! let function = e.finish(0)?;
//! assert_eq!(function.arity(), 1);
//! # Ok::<(), silt::diagnostic::Diagnostic>(())
//! ```
//!
//! # Operands
//!
//! An [`Asm`] carries its operands at full width (`usize` counts and
//! slots). `emit` narrows each to its encoded width, and it is the only
//! place that does: an operand that does not fit is a
//! [`Code::CompileLimit`] diagnostic at the instruction's span, never a
//! wrapped value. A constant operand is a [`Const`] from
//! [`Emitter::constant`]; what kind of constant an instruction needs (a
//! string for `GetFieldNamed`, a function for `MakeClosure`) is in the
//! instruction table ([`ops`](super::ops)) and checked by the verifier.
//!
//! # Height
//!
//! The emitter keeps the height of the frame: it starts at the arity
//! and every emitted instruction changes it by the instruction's effect
//! in the table. The compiler never sets it. A value the code has just
//! pushed is in slot `height() - 1`, which is how a local gets its slot.
//! An instruction that needs more values than the frame holds is a
//! compiler bug, reported where it is emitted.
//!
//! # Jumps
//!
//! A jump names a [`Label`]. `JumpIfFalse` and `JumpIfTrue` go forward:
//! they are emitted before their label is bound. `Jump` goes either
//! way: to a label bound later, or back to one bound already (a loop),
//! which it must reach with the height the label was bound at. The
//! emitter remembers the height each forward jump arrives with, and
//! `bind` sets the height to it. Where jumps arrive at one label with
//! different heights (a failed pattern test leaves the sub-values it
//! was looking at in the frame, an arm leaves its bindings under its
//! result), the height after `bind` is the smallest of them: the values
//! every path has. The verifier lets such a place be followed only by
//! the instructions that put the frame right again or leave it (see
//! [`verify`](mod@super::verify)); the compiler emits `Slide` there.
//!
//! # Unreachable code
//!
//! After `Return`, `Panic` and `Jump` the next instruction is
//! unreachable until a label that a reachable jump goes to is bound. The
//! compiler may keep emitting (`1 + return 2` has an `Add` nobody
//! runs). In unreachable code nothing is checked, and the height is a
//! nominal one: instructions still move it by their effects, and the
//! compiler says what it would be with [`Emitter::assume_height`] where
//! it knows (after an expression that does not return, one more than
//! before it), so that slots of locals in dead code are what they would
//! be in live code.

use crate::diagnostic::{Code, Diagnostic};
use crate::source::Span;
use crate::value::Value;

use super::ops::{Asm, FRAME_VALUES, Flow, Limit, Op, Writer, narrow_u8};
use super::{Chunk, Const, Function, Label, verify};

/// Where a label is, and how jumps get there.
#[derive(Default)]
struct LabelState {
    /// Once bound: its offset, and the height there.
    bound: Option<(usize, usize)>,
    /// The smallest height a reachable jump arrives with.
    arriving: Option<usize>,
    /// The offsets of the operands of the jumps waiting for it, and
    /// whether the operand is the four bytes of a `Jump` (or the two of
    /// a conditional jump).
    waiting: Vec<(usize, bool)>,
}

/// See the [module documentation](self).
pub struct Emitter {
    function: Function,
    height: usize,
    reachable: bool,
    /// Whether the last instruction emitted ends the function's run
    /// (`Return`, `Panic`).
    ended: bool,
    labels: Vec<LabelState>,
    /// The last instruction emitted, when the next one may be fused
    /// with it: no label is bound since (see [`Emitter::emit`]).
    last: Option<Last>,
}

/// An instruction that the next one can be fused with, where it starts,
/// what the height was before it and the span it is blamed on.
#[derive(Clone, Copy)]
struct Last {
    first: First,
    at: usize,
    height: usize,
    span: Span,
}

/// An instruction that is the first of a fused pair.
#[derive(Clone, Copy)]
enum First {
    /// `Constant k` of an Int.
    Int(Const),
    /// A comparison.
    Compare(Op),
}

impl First {
    /// What `asm` is as the first of a pair, if it is one.
    fn of(asm: &Asm<'_>, chunk: &Chunk) -> Option<First> {
        match *asm {
            Asm::Constant { k } if matches!(chunk.constant(k), Value::Int(_)) => {
                Some(First::Int(k))
            }
            Asm::Lt | Asm::Gt | Asm::Leq | Asm::Geq | Asm::Eq | Asm::Neq => {
                Some(First::Compare(asm.op()))
            }
            _ => None,
        }
    }

    /// The one instruction that this and `next` are, if they fuse, and
    /// whether it is blamed on this one's span (else on `next`'s).
    fn fused<'a>(self, next: &Asm<'a>) -> Option<(Asm<'a>, bool)> {
        match (self, *next) {
            (First::Int(k), Asm::Add) => Some((Asm::AddConst { k }, false)),
            (First::Int(k), Asm::Sub) => Some((Asm::SubConst { k }, false)),
            // A comparison that fails is the comparison's error.
            (First::Compare(op), Asm::JumpIfFalse { to }) => Some((jump_unless(op, to), true)),
            // Every comparison orders its operands by one total order
            // (equality by one equivalence): its negation is the
            // other comparison.
            (First::Compare(op), Asm::JumpIfTrue { to }) => {
                let negation = match op {
                    Op::Lt => Op::Geq,
                    Op::Gt => Op::Leq,
                    Op::Leq => Op::Gt,
                    Op::Geq => Op::Lt,
                    Op::Eq => Op::Neq,
                    _ => Op::Eq,
                };
                Some((jump_unless(negation, to), true))
            }
            _ => None,
        }
    }
}

/// The jump forward to `to` unless the comparison `op` holds.
fn jump_unless<'a>(op: Op, to: Label) -> Asm<'a> {
    match op {
        Op::Lt => Asm::JumpIfNotLt { to },
        Op::Gt => Asm::JumpIfNotGt { to },
        Op::Leq => Asm::JumpIfNotLeq { to },
        Op::Geq => Asm::JumpIfNotGeq { to },
        Op::Eq => Asm::JumpIfNotEq { to },
        _ => Asm::JumpIfNotNeq { to },
    }
}

impl Emitter {
    /// An emitter for a function named `name` that takes `arity`
    /// arguments; more than a function can take is a compile limit at
    /// `span`.
    pub fn new(name: String, arity: usize, span: Span) -> Result<Self, Diagnostic> {
        let narrowed = narrow_u8(arity, "parameters of a function")
            .map_err(|limit| limit_diagnostic(limit, span))?;
        Ok(Emitter {
            function: Function {
                name,
                arity: narrowed,
                upvalue_count: 0,
                chunk: Chunk::new(),
            },
            height: arity,
            reachable: true,
            ended: false,
            labels: Vec::new(),
            last: None,
        })
    }

    /// The name of the function being emitted.
    pub fn name(&self) -> &str {
        &self.function.name
    }

    /// The number of values in the frame where the next instruction
    /// runs. In unreachable code, a nominal height.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Whether control can reach the next instruction.
    pub fn reachable(&self) -> bool {
        self.reachable
    }

    /// Whether the code so far is complete as a function's: its last
    /// instruction ends the function's run, and control cannot reach
    /// what would come next.
    pub fn ended(&self) -> bool {
        self.ended && !self.reachable
    }

    /// In unreachable code: what the height would be here.
    pub fn assume_height(&mut self, height: usize) {
        debug_assert!(!self.reachable, "the height of reachable code is known");
        if !self.reachable {
            self.height = height;
        }
    }

    /// The constant `value` of the function.
    pub fn constant(&mut self, value: Value, span: Span) -> Result<Const, Diagnostic> {
        let count = self.function.chunk.constants.len() + 1;
        self.function.chunk.add_constant(value).ok_or_else(|| {
            let limit = Limit {
                what: "constants in a function (its distinct literals, names and functions)",
                count,
                max: usize::from(u16::MAX) + 1,
            };
            limit_diagnostic(limit, span)
        })
    }

    /// Append the instruction `asm`, blamed on `span`.
    ///
    /// Some pairs of instructions are written as one, a superinstruction,
    /// when the second follows the first with no label between them
    /// (nothing jumps to the second): `Constant k; Add` of an Int `k` is
    /// `AddConst k`, and `Sub` likewise, blamed on the `Add`'s span; a
    /// comparison and a conditional jump after it are one jump that
    /// compares (`Lt; JumpIfFalse` is `JumpIfNotLt`), blamed on the
    /// comparison's span.
    pub fn emit(&mut self, asm: Asm<'_>, span: Span) -> Result<(), Diagnostic> {
        let fused = self
            .last
            .and_then(|last| Some((last, last.first.fused(&asm)?)));
        match fused {
            Some((last, (fused, on_first))) => {
                self.function.chunk.truncate(last.at);
                self.height = last.height;
                self.write(fused, if on_first { last.span } else { span })
            }
            None => self.write(asm, span),
        }
    }

    /// [`Emitter::emit`] of an instruction as it is.
    fn write(&mut self, asm: Asm<'_>, span: Span) -> Result<(), Diagnostic> {
        let (at, before) = (self.function.chunk.len(), self.height);
        let effect = asm.effect();
        let op = asm.op().name();
        if self.reachable && effect.pops > self.height {
            return Err(self.bug(
                span,
                format!(
                    "`{op}` takes {} values off a frame of {}",
                    effect.pops, self.height
                ),
            ));
        }
        let popped = self.height.saturating_sub(effect.pops);
        if let Some(cut) = effect.cut
            && self.reachable
            && cut > popped
        {
            return Err(self.bug(
                span,
                format!("`{op}` cuts a frame of {popped} values back to {cut}"),
            ));
        }
        let mut out = Out {
            chunk: &mut self.function.chunk,
            labels: &mut self.labels,
            span,
            arriving: self.reachable.then_some(popped),
            bug: None,
        };
        asm.encode(&mut out)
            .map_err(|limit| limit_diagnostic(limit, span))?;
        if let Some(bug) = out.bug {
            return Err(self.bug(span, format!("`{op}` {bug}")));
        }
        self.height = effect.cut.unwrap_or(popped) + effect.pushes;
        if matches!(effect.flow, Flow::Jump | Flow::End) {
            self.reachable = false;
        }
        self.ended = matches!(effect.flow, Flow::End);
        self.last = First::of(&asm, &self.function.chunk).map(|first| Last {
            first,
            at,
            height: before,
            span,
        });
        Ok(())
    }

    /// A new label, to be bound.
    pub fn label(&mut self) -> Label {
        self.labels.push(LabelState::default());
        Label(self.labels.len() - 1)
    }

    /// Place `label` at the next instruction: the jumps waiting for it
    /// go here.
    pub fn bind(&mut self, label: Label, span: Span) -> Result<(), Diagnostic> {
        self.bind_over(label, "bytes of code in a branch", span)
    }

    /// [`Emitter::bind`], saying what the jumps to `label` go over: what
    /// a program has too much of (`"bytes of code in a match arm"`)
    /// when a conditional jump, which reaches 65,535 bytes forward,
    /// cannot get here. `span` is blamed then.
    pub fn bind_over(
        &mut self,
        label: Label,
        over: &'static str,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let here = self.function.chunk.len();
        self.last = None;
        if self.labels[label.0].bound.is_some() {
            return Err(self.bug(span, "a label is bound twice".into()));
        }
        let state = &mut self.labels[label.0];
        let code = &mut self.function.chunk.code;
        for (operand, wide) in std::mem::take(&mut state.waiting) {
            if wide {
                let count = here - (operand + 4);
                let distance = i32::try_from(count)
                    .map_err(|_| limit_diagnostic(function_too_large(count), span))?;
                code[operand..operand + 4].copy_from_slice(&distance.to_le_bytes());
            } else {
                let count = here - (operand + 2);
                let distance = u16::try_from(count).map_err(|_| {
                    let limit = Limit {
                        what: over,
                        count,
                        max: usize::from(u16::MAX),
                    };
                    limit_diagnostic(limit, span)
                })?;
                code[operand..operand + 2].copy_from_slice(&distance.to_le_bytes());
            }
        }
        if let Some(arriving) = state.arriving {
            self.height = match self.reachable {
                true => self.height.min(arriving),
                false => arriving,
            };
            self.reachable = true;
        }
        state.bound = Some((here, self.height));
        Ok(())
    }

    /// The function, with `upvalue_count` upvalues (more than a closure
    /// can capture is a compile limit). Its code is verified: malformed
    /// code is a compiler bug, reported here and never run.
    pub fn finish(mut self, upvalue_count: usize) -> Result<Function, Diagnostic> {
        let span = self
            .function
            .chunk
            .spans
            .first()
            .map_or(Span::BUILTIN, |(_, span)| *span);
        if self
            .labels
            .iter()
            .any(|label| label.bound.is_none() && !label.waiting.is_empty())
        {
            return Err(self.bug(span, "a jump goes to a label that is never bound".into()));
        }
        self.function.upvalue_count =
            narrow_u8(upvalue_count, CAPTURES).map_err(|limit| limit_diagnostic(limit, span))?;
        match verify(&self.function) {
            Ok(()) => Ok(self.function),
            Err(error) => Err(self.bug(span, error.to_string())),
        }
    }

    fn bug(&self, span: Span, what: String) -> Diagnostic {
        Diagnostic::error(
            Code::CompilerBug,
            span,
            format!(
                "compiler bug: malformed code for '{}': {what}",
                self.function.name
            ),
        )
    }
}

/// What an instruction's encoder writes to.
struct Out<'e> {
    chunk: &'e mut Chunk,
    labels: &'e mut Vec<LabelState>,
    span: Span,
    /// The height a jump arrives with; `None` in unreachable code.
    arriving: Option<usize>,
    /// What is wrong with a jump, if something is.
    bug: Option<&'static str>,
}

impl Writer for Out<'_> {
    fn u8(&mut self, byte: u8) {
        self.chunk.push(byte, self.span);
    }

    fn fwd(&mut self, to: Label) {
        let state = &mut self.labels[to.0];
        if state.bound.is_some() {
            self.bug = Some("jumps forward to a label that is behind it");
        }
        state.waiting.push((self.chunk.len(), false));
        if let Some(arriving) = self.arriving {
            state.arriving = Some(state.arriving.map_or(arriving, |known| known.min(arriving)));
        }
        self.u16(u16::MAX);
    }

    fn rel(&mut self, to: Label) -> Result<(), Limit> {
        let state = &mut self.labels[to.0];
        let distance = match state.bound {
            // Forward: the label's `bind` writes the distance.
            None => {
                state.waiting.push((self.chunk.len(), true));
                if let Some(arriving) = self.arriving {
                    state.arriving =
                        Some(state.arriving.map_or(arriving, |known| known.min(arriving)));
                }
                0
            }
            // Back, from the end of this operand.
            Some((target, height)) => {
                if self.arriving.is_some_and(|arriving| arriving != height) {
                    self.bug = Some("jumps back with a height the loop did not start with");
                }
                let count = self.chunk.len() + 4 - target;
                let back = i32::try_from(count).map_err(|_| function_too_large(count))?;
                -back
            }
        };
        for byte in distance.to_le_bytes() {
            self.u8(byte);
        }
        Ok(())
    }
}

/// What a closure has too many of when its upvalue count does not fit.
const CAPTURES: &str = "values a closure captures";

/// A `Jump` over `count` bytes, more than its operand can say.
fn function_too_large(count: usize) -> Limit {
    Limit {
        what: "bytes of code in a function",
        count,
        max: i32::MAX.unsigned_abs() as usize,
    }
}

/// The compile error for a limit of the bytecode: the one wording every
/// limit has.
pub fn limit_diagnostic(limit: Limit, span: Span) -> Diagnostic {
    let Limit { what, count, max } = limit;
    let diagnostic = Diagnostic::error(
        Code::CompileLimit,
        span,
        format!("too many {what}: {count} (the limit is {max})"),
    );
    match what == FRAME_VALUES {
        true => diagnostic.with_help(
            "move some of the function's statements into separate functions, \
             or split a large expression into smaller parts",
        ),
        false => diagnostic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::ops::Op;

    fn span() -> Span {
        Span::BUILTIN
    }

    #[test]
    fn height_follows_the_table() {
        let mut e = Emitter::new("f".into(), 2, span()).unwrap();
        assert_eq!(e.height(), 2);
        e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
        e.emit(Asm::GetLocal { slot: 1 }, span()).unwrap();
        assert_eq!(e.height(), 4);
        e.emit(Asm::Add, span()).unwrap();
        assert_eq!(e.height(), 3);
        e.emit(Asm::Slide { slot: 0 }, span()).unwrap();
        assert_eq!(e.height(), 1);
        e.emit(Asm::Return, span()).unwrap();
        assert!(!e.reachable());
        let f = e.finish(0).unwrap();
        let ops: Vec<Op> = f.chunk().instrs().map(|(_, instr)| instr.op()).collect();
        assert_eq!(
            ops,
            [Op::GetLocal, Op::GetLocal, Op::Add, Op::Slide, Op::Return]
        );
    }

    #[test]
    fn an_operand_that_does_not_fit_is_a_compile_limit() {
        let mut e = Emitter::new("f".into(), 0, span()).unwrap();
        e.emit(Asm::Unit, span()).unwrap();
        let err = e.emit(Asm::TestListExact { len: 256 }, span()).unwrap_err();
        assert_eq!(err.code, Code::CompileLimit);
        assert_eq!(
            err.message,
            "too many elements of a list pattern: 256 (the limit is 255)"
        );
        let err = e.emit(Asm::GetLocal { slot: 65_536 }, span()).unwrap_err();
        assert_eq!(err.code, Code::CompileLimit);
        assert!(
            err.message
                .starts_with("too many values in the frame of a function"),
            "{err:?}"
        );
        assert!(
            err.message.ends_with(": 65537 (the limit is 65536)"),
            "{err:?}"
        );
        assert_eq!(err.help.len(), 1);
    }

    #[test]
    fn what_a_function_and_a_closure_can_hold_are_compile_limits() {
        let err = Emitter::new("f".into(), 256, span()).err().unwrap();
        assert_eq!(err.code, Code::CompileLimit);
        assert_eq!(
            err.message,
            "too many parameters of a function: 256 (the limit is 255)"
        );

        let mut e = Emitter::new("f".into(), 0, span()).unwrap();
        e.emit(Asm::GetUpvalue { index: 255 }, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
        let err = e.finish(256).unwrap_err();
        assert_eq!(err.code, Code::CompileLimit);
        assert_eq!(
            err.message,
            "too many values a closure captures: 256 (the limit is 255)"
        );

        let inner = Function::returning_unit("inner".into(), 0);
        let mut e = Emitter::new("f".into(), 0, span()).unwrap();
        let f = e
            .constant(
                Value::VmClosure(std::sync::Arc::new(crate::bytecode::VmClosure {
                    function: std::sync::Arc::new(inner),
                    upvalues: vec![],
                })),
                span(),
            )
            .unwrap();
        let captures = [crate::bytecode::UpvalueDesc {
            is_local: true,
            index: 256,
        }];
        let err = e
            .emit(
                Asm::MakeClosure {
                    f,
                    captures: &captures,
                },
                span(),
            )
            .unwrap_err();
        assert_eq!(
            err.message,
            "too many values in the frame below a local that a closure captures: \
             256 (the limit is 255)"
        );
    }

    #[test]
    fn an_instruction_that_underflows_the_frame_is_a_compiler_bug() {
        let mut e = Emitter::new("f".into(), 0, span()).unwrap();
        e.emit(Asm::Unit, span()).unwrap();
        let err = e.emit(Asm::Add, span()).unwrap_err();
        assert_eq!(err.code, Code::CompilerBug);
        assert!(
            err.message
                .contains("`Add` takes 2 values off a frame of 1")
        );
    }

    #[test]
    fn a_label_takes_the_height_of_the_jumps_to_it() {
        let mut e = Emitter::new("f".into(), 1, span()).unwrap();
        let other = e.label();
        let end = e.label();
        e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
        e.emit(Asm::JumpIfFalse { to: other }, span()).unwrap();
        e.emit(Asm::True, span()).unwrap();
        e.emit(Asm::Jump { to: end }, span()).unwrap();
        assert!(!e.reachable());
        e.bind(other, span()).unwrap();
        assert!(e.reachable());
        assert_eq!(e.height(), 1);
        e.emit(Asm::False, span()).unwrap();
        e.bind(end, span()).unwrap();
        assert_eq!(e.height(), 2);
        e.emit(Asm::Return, span()).unwrap();
        e.finish(0).unwrap();
    }

    #[test]
    fn unreachable_code_is_emitted_unchecked_at_the_assumed_height() {
        let mut e = Emitter::new("f".into(), 0, span()).unwrap();
        e.emit(Asm::Unit, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
        e.assume_height(1);
        e.emit(Asm::Add, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
        e.finish(0).unwrap();
    }

    #[test]
    fn a_jump_back_must_arrive_with_the_height_of_its_label() {
        let mut e = Emitter::new("f".into(), 0, span()).unwrap();
        let start = e.label();
        e.bind(start, span()).unwrap();
        e.emit(Asm::Unit, span()).unwrap();
        let err = e.emit(Asm::Jump { to: start }, span()).unwrap_err();
        assert_eq!(err.code, Code::CompilerBug);
    }

    #[test]
    fn a_conditional_jump_over_more_than_65535_bytes_is_a_compile_limit() {
        let mut e = Emitter::new("f".into(), 0, span()).unwrap();
        let far = e.label();
        let end = e.label();
        e.emit(Asm::True, span()).unwrap();
        e.emit(Asm::JumpIfFalse { to: far }, span()).unwrap();
        for _ in 0..40_000 {
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Pop, span()).unwrap();
        }
        // The unconditional jump reaches as far as code goes.
        e.emit(Asm::Jump { to: end }, span()).unwrap();
        let err = e.bind(far, span()).unwrap_err();
        assert_eq!(err.code, Code::CompileLimit);
        assert_eq!(
            err.message,
            "too many bytes of code in a branch: 80005 (the limit is 65535)"
        );
    }

    #[test]
    fn a_jump_reaches_back_over_more_than_65535_bytes() {
        let mut e = Emitter::new("f".into(), 0, span()).unwrap();
        let start = e.label();
        e.bind(start, span()).unwrap();
        for _ in 0..40_000 {
            e.emit(Asm::Unit, span()).unwrap();
            e.emit(Asm::Pop, span()).unwrap();
        }
        e.emit(Asm::Jump { to: start }, span()).unwrap();
        let f = e.finish(0).unwrap();
        let (at, last) = f.chunk().instrs().last().unwrap();
        assert_eq!(at, 80_000);
        assert!(matches!(last, crate::bytecode::Instr::Jump { to: 0 }));
    }

    /// The ops of the code `emit` writes into a function of one
    /// parameter that returns what it leaves.
    fn fused_ops(emit: impl FnOnce(&mut Emitter)) -> Vec<Op> {
        let mut e = Emitter::new("f".into(), 1, span()).unwrap();
        emit(&mut e);
        e.emit(Asm::Return, span()).unwrap();
        let f = e.finish(0).unwrap();
        f.chunk().instrs().map(|(_, instr)| instr.op()).collect()
    }

    #[test]
    fn an_int_constant_and_an_add_or_a_sub_are_one_instruction() {
        let at = |start| Span {
            start,
            end: start + 1,
            ..span()
        };
        let mut e = Emitter::new("f".into(), 1, span()).unwrap();
        let one = e.constant(Value::Int(1), span()).unwrap();
        e.emit(Asm::GetLocal { slot: 0 }, at(1)).unwrap();
        e.emit(Asm::Constant { k: one }, at(2)).unwrap();
        e.emit(Asm::Add, at(3)).unwrap();
        assert_eq!(e.height(), 2);
        e.emit(Asm::Constant { k: one }, at(4)).unwrap();
        e.emit(Asm::Sub, at(5)).unwrap();
        e.emit(Asm::Return, at(6)).unwrap();
        let f = e.finish(0).unwrap();
        let instrs: Vec<_> = f.chunk().instrs().collect();
        let ops: Vec<Op> = instrs.iter().map(|(_, instr)| instr.op()).collect();
        assert_eq!(ops, [Op::GetLocal, Op::AddConst, Op::SubConst, Op::Return]);
        // Each is blamed on the span of its `Add` or `Sub`.
        let spans: Vec<u32> = instrs
            .iter()
            .map(|(offset, _)| f.chunk().span_at(*offset).start)
            .collect();
        assert_eq!(spans, [1, 3, 5, 6]);
    }

    #[test]
    fn a_constant_that_is_no_int_is_not_fused() {
        let ops = fused_ops(|e| {
            let half = e.constant(Value::Float(crate::value::Float::new(0.5).unwrap()), span());
            e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
            e.emit(Asm::Constant { k: half.unwrap() }, span()).unwrap();
            e.emit(Asm::Add, span()).unwrap();
        });
        assert_eq!(ops, [Op::GetLocal, Op::Constant, Op::Add, Op::Return]);
    }

    #[test]
    fn nothing_is_fused_across_a_label() {
        // `n + (match c { true -> 1, false -> 2 })`: the `Add` is where
        // both arms go on, so the second arm's constant is not its
        // operand on the first's path.
        let ops = fused_ops(|e| {
            let (one, two) = (
                e.constant(Value::Int(1), span()),
                e.constant(Value::Int(2), span()),
            );
            let (other, end) = (e.label(), e.label());
            e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
            e.emit(Asm::True, span()).unwrap();
            e.emit(Asm::JumpIfFalse { to: other }, span()).unwrap();
            e.emit(Asm::Constant { k: one.unwrap() }, span()).unwrap();
            e.emit(Asm::Jump { to: end }, span()).unwrap();
            e.bind(other, span()).unwrap();
            e.emit(Asm::Constant { k: two.unwrap() }, span()).unwrap();
            e.bind(end, span()).unwrap();
            e.emit(Asm::Add, span()).unwrap();
        });
        assert_eq!(
            ops,
            [
                Op::GetLocal,
                Op::True,
                Op::JumpIfFalse,
                Op::Constant,
                Op::Jump,
                Op::Constant,
                Op::Add,
                Op::Return
            ]
        );
    }

    #[test]
    fn a_comparison_and_a_conditional_jump_are_one_jump() {
        let cases = [
            (Asm::Lt, Op::JumpIfNotLt, Op::JumpIfNotGeq),
            (Asm::Gt, Op::JumpIfNotGt, Op::JumpIfNotLeq),
            (Asm::Leq, Op::JumpIfNotLeq, Op::JumpIfNotGt),
            (Asm::Geq, Op::JumpIfNotGeq, Op::JumpIfNotLt),
            (Asm::Eq, Op::JumpIfNotEq, Op::JumpIfNotNeq),
            (Asm::Neq, Op::JumpIfNotNeq, Op::JumpIfNotEq),
        ];
        let at = |start| Span {
            start,
            end: start + 1,
            ..span()
        };
        for (compare, if_false, if_true) in cases {
            for (on_true, fused) in [(false, if_false), (true, if_true)] {
                let mut e = Emitter::new("f".into(), 2, span()).unwrap();
                let end = e.label();
                e.emit(Asm::GetLocal { slot: 0 }, at(1)).unwrap();
                e.emit(Asm::GetLocal { slot: 1 }, at(2)).unwrap();
                e.emit(compare, at(3)).unwrap();
                let jump = match on_true {
                    true => Asm::JumpIfTrue { to: end },
                    false => Asm::JumpIfFalse { to: end },
                };
                e.emit(jump, at(4)).unwrap();
                assert_eq!(e.height(), 2);
                e.emit(Asm::Unit, at(5)).unwrap();
                e.emit(Asm::Pop, at(5)).unwrap();
                e.bind(end, span()).unwrap();
                e.emit(Asm::Unit, at(6)).unwrap();
                e.emit(Asm::Return, at(6)).unwrap();
                let f = e.finish(0).unwrap();
                let instrs: Vec<_> = f.chunk().instrs().collect();
                let (offset, instr) = instrs[2];
                assert_eq!(instr.op(), fused, "{compare:?}, jump if {on_true}");
                // Blamed on the comparison.
                assert_eq!(f.chunk().span_at(offset).start, 3);
                // It jumps over the `Unit; Pop`.
                assert!(matches!(instr.effect().flow, Flow::Branch));
                assert_eq!(instrs[3].1.op(), Op::Unit);
            }
        }
    }

    #[test]
    fn a_comparison_where_paths_meet_is_not_fused_with_the_jump() {
        // `if (match c { true -> x, false -> a < b }) ...`: the jump is
        // where both arms go on, and the first arm's value is no
        // comparison of its own.
        let ops = fused_ops(|e| {
            let (other, end, after) = (e.label(), e.label(), e.label());
            e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
            e.emit(Asm::JumpIfFalse { to: other }, span()).unwrap();
            e.emit(Asm::True, span()).unwrap();
            e.emit(Asm::Jump { to: end }, span()).unwrap();
            e.bind(other, span()).unwrap();
            e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
            e.emit(Asm::GetLocal { slot: 0 }, span()).unwrap();
            e.emit(Asm::Lt, span()).unwrap();
            e.bind(end, span()).unwrap();
            e.emit(Asm::JumpIfFalse { to: after }, span()).unwrap();
            e.bind(after, span()).unwrap();
            e.emit(Asm::Unit, span()).unwrap();
        });
        assert_eq!(
            ops,
            [
                Op::GetLocal,
                Op::JumpIfFalse,
                Op::True,
                Op::Jump,
                Op::GetLocal,
                Op::GetLocal,
                Op::Lt,
                Op::JumpIfFalse,
                Op::Unit,
                Op::Return
            ]
        );
    }

    #[test]
    fn a_jump_to_a_label_never_bound_is_a_compiler_bug() {
        let mut e = Emitter::new("f".into(), 0, span()).unwrap();
        let nowhere = e.label();
        e.emit(Asm::Jump { to: nowhere }, span()).unwrap();
        e.emit(Asm::Unit, span()).unwrap();
        e.emit(Asm::Return, span()).unwrap();
        assert_eq!(e.finish(0).unwrap_err().code, Code::CompilerBug);
    }
}
