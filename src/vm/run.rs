//! Main execution loop and opcode dispatch.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::builtins::registry::registry;
use crate::bytecode::{Chunk, Instr, Op, Segment, Segments, VmClosure, decode};
use crate::scheduler::{Blocks, SliceResult};
use crate::typeinfo::bv;
use crate::value::{Float, List, Record, Value};

use super::arithmetic::{check_same_type, float_arithmetic, int_arithmetic, ordering};
use super::calls::Entered;
use super::runtime::{Frame, Step};
use super::show::write_plain;
use super::{Vm, VmError};
use crate::runtime::sync::Wait;

/// Gate the language-level `==` / `!=` operators against function-shaped
/// values, returning the canonical surface name (`"Fn"`) to name in the
/// error if the value cannot participate in equality, or `None` if it can.
///
/// The checker rejects `==` on a value that is, or holds, a function
/// (`Equal` is decided by structure, for a concrete operand and through
/// a bound alike): this is the backstop at the execution site, where
/// `PartialEq for Value` would answer by function and captures on closures and
/// name-equality on builtins (src/value/key.rs). The recursion is
/// `Value::contains_fn` (src/value/mod.rs). Channel / Handle /
/// TcpListener / TcpStream are deliberately NOT rejected: they are
/// equatable by identity at runtime and have `Equal` in the checker,
/// keeping the runtime and compile-time
/// layers in parity. Rust-level collection keying / dedup uses `PartialEq
/// for Value` directly, not this operator path (see
/// tests/typecheck/round74_hash_eq_ord_contract_tests.rs) — but the silt-visible
/// collection builtins that consume that ordering/equality (list.sort /
/// unique / contains / index_of, set.from_list / insert / contains /
/// remove and the set algebra ops) carry their own mirror of this gate:
/// `ensure_no_fn` in src/builtins/collections.rs, locked by
/// tests/lang/collection_builtin_fn_gate_tests.rs. Locked by
/// tests/lang/round96_eq_fn_runtime_tests.rs and
/// tests/typecheck/container_fn_compare_runtime_gate_tests.rs.
fn equality_operand_violation(val: &Value) -> Option<&'static str> {
    if val.contains_fn() { Some("Fn") } else { None }
}

/// The operands of `==` and `!=`, if the two can be compared so.
fn equality_operands(a: &Value, b: &Value) -> Result<(), VmError> {
    check_same_type(a, b)?;
    // Reject function-shaped operands at the execution site: the
    // typechecker skips this bound on still-polymorphic operands
    // and relies on the VM to catch it (see
    // `equality_operand_violation`). Both operands are checked:
    // the gate recurses into containers, and a shared
    // discriminant no longer implies a shared violation (an
    // empty list and a list of closures both have the List
    // discriminant).
    match equality_operand_violation(a).or_else(|| equality_operand_violation(b)) {
        Some(name) => Err(VmError::type_confusion(format!(
            "type '{name}' does not implement Equal"
        ))),
        None => Ok(()),
    }
}

/// What an instruction did that a method of the VM ran for the loop.
pub(super) enum DispatchResult {
    /// Go on with the next instruction of the function's frame on top
    /// (a call put the callee's frame there).
    Continue,
    /// A builtin's frame is on top, not resumed yet.
    Native,
}

/// What the instruction loop keeps in variables of the function's
/// frame on top of `frames`, the one of the instruction being run: the
/// closure, its function's constants and code, the instruction pointer
/// and the frame's first slot.
#[inline(always)]
fn loaded(frames: &[Frame]) -> (&VmClosure, &Chunk, &[u8], usize, usize) {
    let Some(Frame::Code(frame)) = frames.last() else {
        unreachable!("an instruction runs in a function's frame")
    };
    let chunk = frame.closure.function.chunk();
    (
        &frame.closure,
        chunk,
        chunk.code(),
        frame.ip,
        frame.base_slot,
    )
}

// The code a VM runs has passed the bytecode verifier
// (`crate::bytecode::verify`): an instruction finds the values it
// takes on the stack, its slots in the frame and its constants in
// the pool, of the kinds it needs. The accessors below do not look
// for anything else.

/// Write `value` over `*slot`, for the instructions that set a slot of
/// the stack. The value that was there is let go of where it is: a
/// number, a Bool or unit owns nothing and is written over without a
/// call of drop, a value of any other kind is dropped as an assignment
/// drops it. (The kinds are named: one that is added later does not
/// compile until it is placed here.)
#[inline(always)]
fn put(slot: &mut Value, value: Value) {
    match slot {
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Unit => {
            std::mem::forget(std::mem::replace(slot, value));
        }
        Value::String(_)
        | Value::List(_)
        | Value::Map(_)
        | Value::Set(_)
        | Value::Tuple(_)
        | Value::Record(_)
        | Value::Variant(_)
        | Value::VmClosure(_)
        | Value::BuiltinFn(_)
        | Value::HostFn(_)
        | Value::VariantConstructor(_)
        | Value::TypeDescriptor(_)
        | Value::PrimitiveDescriptor(_)
        | Value::Channel(_)
        | Value::Handle(_)
        | Value::Bytes(_)
        | Value::TcpListener(_)
        | Value::TcpStream(_) => *slot = value,
    }
}

// A new value is written where it goes, and a push makes room first.
// A Value is 40 bytes; one just written in two smaller stores (its tag,
// its number) and then read back in 16-byte pieces, as a move reads
// it, makes the processor wait for the stores, 12 to 14 cycles each
// time. (Measured in pull request #54, 2026-10-12.)

/// A value as the instructions that copy one sort it: a number, a Bool
/// or unit, which owns nothing and is built where it goes; a closure,
/// which is one pointer and is built where it goes too; a value of any
/// other kind, which is cloned and moved.
enum Sort<'a> {
    Int(i64),
    Float(Float),
    Bool(bool),
    Unit,
    Closure(&'a Arc<VmClosure>),
    Other(&'a Value),
}

/// The [`Sort`] of `value`. (The kinds are named: one that is added
/// later does not compile until it is placed here.)
#[inline(always)]
fn sort(value: &Value) -> Sort<'_> {
    match value {
        Value::Int(n) => Sort::Int(*n),
        Value::Float(f) => Sort::Float(*f),
        Value::Bool(b) => Sort::Bool(*b),
        Value::Unit => Sort::Unit,
        Value::VmClosure(closure) => Sort::Closure(closure),
        Value::String(_)
        | Value::List(_)
        | Value::Map(_)
        | Value::Set(_)
        | Value::Tuple(_)
        | Value::Record(_)
        | Value::Variant(_)
        | Value::BuiltinFn(_)
        | Value::HostFn(_)
        | Value::VariantConstructor(_)
        | Value::TypeDescriptor(_)
        | Value::PrimitiveDescriptor(_)
        | Value::Channel(_)
        | Value::Handle(_)
        | Value::Bytes(_)
        | Value::TcpListener(_)
        | Value::TcpStream(_) => Sort::Other(value),
    }
}

/// Make room in `stack` for one more. Rare, and so a call.
#[cold]
#[inline(never)]
fn grow<T>(stack: &mut Vec<T>) {
    stack.reserve(1);
}

/// Push what `make` builds: it is built in its slot of `stack`. (A
/// plain `Vec::push` builds its value somewhere else and copies it,
/// because the value has to outlive the call that makes room. Here the
/// room is made first, and the compiler sees that the push needs none.)
#[inline(always)]
pub(super) fn push_with<T>(stack: &mut Vec<T>, make: impl FnOnce() -> T) {
    if stack.len() == stack.capacity() {
        grow(stack);
    }
    if stack.len() < stack.capacity() {
        stack.push(make());
    } else {
        unreachable!("`grow` made room for one more")
    }
}

// These five are not inlined into the loop: there rustc 1.99.0 calls
// `Vec::push_mut` out of line and copies the value in (measured 2026-10-12).

#[inline(never)]
fn push_int(stack: &mut Vec<Value>, n: i64) {
    push_with(stack, || Value::Int(n));
}

#[inline(never)]
fn push_float(stack: &mut Vec<Value>, f: Float) {
    push_with(stack, || Value::Float(f));
}

#[inline(never)]
fn push_bool(stack: &mut Vec<Value>, b: bool) {
    push_with(stack, || Value::Bool(b));
}

#[inline(never)]
fn push_unit(stack: &mut Vec<Value>) {
    push_with(stack, || Value::Unit);
}

#[inline(never)]
fn push_closure(stack: &mut Vec<Value>, closure: Arc<VmClosure>) {
    push_with(stack, move || Value::VmClosure(closure));
}

/// Push a copy of the value in the place `$value`, for the
/// instructions that read a slot, a constant or a global.
macro_rules! push_copy {
    ($stack:expr, $value:expr) => {
        match sort(&$value) {
            Sort::Int(n) => push_int(&mut $stack, n),
            Sort::Float(f) => push_float(&mut $stack, f),
            Sort::Bool(b) => push_bool(&mut $stack, b),
            Sort::Unit => push_unit(&mut $stack),
            Sort::Closure(closure) => {
                let closure = closure.clone();
                push_closure(&mut $stack, closure);
            }
            Sort::Other(value) => {
                let value = value.clone();
                $stack.push(value);
            }
        }
    };
}

/// Write a copy of the value in the place `$value` over slot `$to` of
/// the stack ([`put`]), for the instructions that set a slot from
/// another, or from a part of the value that is in it: the copy is
/// made before the slot's old value is let go of.
macro_rules! set_copy {
    ($stack:expr, $to:expr, $value:expr) => {{
        let to = $to;
        match sort(&$value) {
            Sort::Int(n) => put(&mut $stack[to], Value::Int(n)),
            Sort::Float(f) => put(&mut $stack[to], Value::Float(f)),
            Sort::Bool(b) => put(&mut $stack[to], Value::Bool(b)),
            Sort::Unit => put(&mut $stack[to], Value::Unit),
            Sort::Closure(_) | Sort::Other(_) => {
                let value = $value.clone();
                put(&mut $stack[to], value);
            }
        }
    }};
}

/// Make a slot that holds unit below the stack's slot `at`: the values
/// from `at` on go one slot up, the highest first, and unit is written
/// where each was. (`Vec::insert` builds the unit somewhere else and
/// copies it in.)
#[inline(always)]
fn open_slot(stack: &mut Vec<Value>, at: usize) {
    let top = stack.len();
    push_unit(stack);
    for i in (at..top).rev() {
        let value = std::mem::replace(&mut stack[i], Value::Unit);
        put(&mut stack[i + 1], value);
    }
}

/// Take the value on top off the stack and let go of it where it
/// lies: a number, a Bool or unit, which owns nothing, is not moved
/// out first and not handed to the drop of a value of any kind.
#[inline(always)]
fn pop_done(stack: &mut Vec<Value>) {
    match sort(peek(stack)) {
        Sort::Int(_) | Sort::Float(_) | Sort::Bool(_) | Sort::Unit => {
            std::mem::forget(stack.pop());
        }
        Sort::Closure(_) | Sort::Other(_) => drop(stack.pop()),
    }
}

/// `Interpolate`: put the String of `segments` in place of the values
/// of the segments on top of `stack`. A lone segment that is a String
/// is the String itself: nothing is copied. Otherwise the string is
/// written in `text`, the VM's buffer, and copied once into the String
/// it is. A literal is read in place: its count is not touched, which
/// tasks running the same code share. `Err` names the slot of a value
/// that is none of those `DisplayValue` leaves (`write_plain`).
///
/// A call, not a part of the loop: the loop's code is kept to what it
/// was, which the speed of every program hangs on.
#[inline(never)]
fn interpolate(
    stack: &mut Vec<Value>,
    text: &mut String,
    segments: Segments,
    code: &[u8],
    chunk: &Chunk,
) -> Result<(), usize> {
    let start = stack.len() - segments.values();
    if segments.len() == 1 && segments.values() == 1 && matches!(stack[start], Value::String(_)) {
        return Ok(());
    }
    let mut buffer = std::mem::take(text);
    buffer.clear();
    let mut next = start;
    for segment in segments.iter(code) {
        match segment {
            Segment::Literal(k) => buffer.push_str(chunk.string(k)),
            Segment::Value => {
                if !write_plain(&mut buffer, &stack[next]) {
                    return Err(next);
                }
                next += 1;
            }
        }
    }
    stack.truncate(start);
    stack.push(Value::String(buffer.as_str().into()));
    // (A buffer an outsized string grew is not kept.)
    if buffer.capacity() <= KEPT_TEXT {
        *text = buffer;
    }
    Ok(())
}

/// The most bytes of the buffer `Interpolate` writes in that a VM
/// keeps for the next one ([`Vm`]'s `text`). Small: a program can have
/// a hundred thousand VMs, each holding its buffer while it lives.
const KEPT_TEXT: usize = 256;

#[inline(always)]
fn pop(stack: &mut Vec<Value>) -> Value {
    stack.pop().expect("verified code pops a value it pushed")
}

#[inline(always)]
fn peek(stack: &[Value]) -> &Value {
    stack
        .last()
        .expect("verified code looks at a value it pushed")
}

/// How a run of the instruction loop ended ([`Vm::run_frames`]).
pub(super) enum Slice {
    /// The frames above the floor are finished, with this value.
    Done(Value),
    /// The budget is used up; the frames can go on.
    OutOfBudget,
    /// A builtin gave the rest of the slice away ([`Step::Yield`]); the
    /// frames can go on.
    Yielded,
    /// A builtin's frame waits for this (see [`Step::Park`]).
    Parked(Wait),
}

impl Vm {
    // ── The loop ──────────────────────────────────────────────────

    /// Run the frames above the first `floor`, which are finished when
    /// the function or builtin in frame `floor` has returned, for at
    /// most `budget` steps: instructions, and resumptions of a
    /// builtin's frame.
    ///
    /// This is the only loop that runs silt code. A call, whoever makes
    /// it (an instruction, a method call, a builtin that was passed a
    /// function), is a frame above the caller's, so the depth of silt
    /// calls costs frames and never host stack. The loop is entered
    /// with the frames as an earlier run left them, when that one ran
    /// out of budget or parked.
    ///
    /// On an error the frames stay as they are, for the caller to read
    /// the call stack from ([`Vm::enrich_error`]) and to drop
    /// ([`Vm::unwind`]).
    pub(super) fn run_frames(&mut self, floor: usize, mut budget: usize) -> Result<Slice, VmError> {
        // A builtin's frame on top waits for its input: the value an
        // earlier run had for it when the budget ran out, or unit.
        if self.frames.len() == floor {
            return Ok(Slice::Done(Value::Unit));
        }
        if let Some(Frame::Native(_)) = self.frames.last() {
            let input = self.pending_input.take().unwrap_or(Value::Unit);
            let mut left = budget;
            let end = self.deliver(floor, input, &mut left)?;
            budget = left;
            if let Some(end) = end {
                return Ok(end);
            }
        }

        // The loop keeps the state of the function's frame on top in
        // variables of its own: the closure, its constants and its
        // code, the instruction pointer and the frame's first slot. An
        // instruction that works on these and on the stack touches no
        // frame.
        //
        // The frame's own `ip` is behind while the loop runs. It is
        // written back (`save!`) before the loop is left, and before
        // anything else reads the frames or puts one on top: a call
        // (the caller's `ip` is where it goes on), a tail call (the
        // log), a builtin, a method of the VM that can fail (an error's
        // place is read off the frames). The variables are read again
        // (`load!`) when the frame on top may be another one, and after
        // every method that takes the whole VM: they borrow from the
        // frames, so the compiler asks for it.
        let (mut closure, mut chunk, mut code, mut ip, mut base) = loaded(&self.frames);
        macro_rules! load {
            () => {
                (closure, chunk, code, ip, base) = loaded(&self.frames)
            };
        }
        macro_rules! save {
            () => {
                self.frame_mut().ip = ip
            };
        }
        // An error of the instruction being run. Its frame is on top:
        // nothing that fails in the loop has changed the frames.
        macro_rules! fail {
            ($error:expr) => {{
                let error = $error;
                save!();
                return Err(error);
            }};
        }
        macro_rules! ok {
            ($result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(error) => fail!(error),
                }
            };
        }
        // Give a value to the frame on top, which need not be a
        // function's, and go on with the function's frame that is on
        // top after that. (An error here has the frames as `deliver`
        // left them, each with its `ip`: it is not `fail!`'s.)
        macro_rules! deliver {
            ($value:expr) => {{
                // (A copy, so that the loop's own count stays in a
                // register.)
                let mut left = budget;
                let end = self.deliver(floor, $value, &mut left)?;
                budget = left;
                if let Some(end) = end {
                    return Ok(end);
                }
                load!();
            }};
        }
        // The frame on top is finished, with this value: `Return`, or
        // `?` on an `Err` or a `None`.
        macro_rules! returned {
            ($value:expr) => {{
                let value = $value;
                self.frames.pop();
                // Prune any tail-call elided diagnostic entries that
                // belong to the just-popped frame slot so stale data
                // can't bleed into later unrelated calls at this depth.
                if !self.tco_elided.is_empty() {
                    self.tco_elided.prune(self.frames.len());
                }
                // The function's own slot goes with its frame.
                self.stack.truncate(base.saturating_sub(1));
                deliver!(value);
            }};
        }
        // Go on after an instruction that a method of the VM ran.
        macro_rules! go_on {
            ($result:expr) => {
                match $result {
                    DispatchResult::Continue => load!(),
                    DispatchResult::Native => deliver!(Value::Unit),
                }
            };
        }
        // An instruction of two operands reads them where they lie, on
        // top of the stack, and writes its result over the first; the
        // second is let go of. Nothing is taken off the stack and
        // pushed again.
        macro_rules! operands {
            () => {{
                let [.., a, b] = self.stack.as_mut_slice() else {
                    unreachable!("verified code has both operands on the stack")
                };
                (a, b)
            }};
        }
        // The instruction refuses its operands: they leave the stack,
        // the first before the second, and the instruction fails.
        macro_rules! refuse {
            ($error:expr) => {{
                let error = $error;
                let below = self.stack.len() - 2;
                self.stack.truncate(below);
                fail!(error)
            }};
        }
        macro_rules! arithmetic {
            ($op:expr) => {{
                let (a, b) = operands!();
                if let (Value::Int(x), Value::Int(y)) = (&*a, &*b) {
                    match int_arithmetic($op, *x, *y) {
                        Ok(n) => put(a, Value::Int(n)),
                        Err(error) => refuse!(error),
                    }
                } else {
                    match float_arithmetic($op, a, b) {
                        Ok(f) => put(a, Value::Float(f)),
                        Err(error) => refuse!(error),
                    }
                }
                pop_done(&mut self.stack);
            }};
        }
        macro_rules! compare {
            ($holds:expr) => {{
                let (a, b) = operands!();
                match ordering(a, b) {
                    Ok(ordering) => put(a, Value::Bool($holds(ordering))),
                    Err(error) => refuse!(error),
                }
                pop_done(&mut self.stack);
            }};
        }

        loop {
            if budget == 0 {
                save!();
                return Ok(Slice::OutOfBudget);
            }
            budget -= 1;
            let (instr, next) = decode(code, ip).expect("verified code decodes");
            ip = next;
            match instr {
                Instr::Constant { k } => push_copy!(self.stack, *chunk.constant(k)),
                Instr::Unit => push_unit(&mut self.stack),
                Instr::True => push_bool(&mut self.stack, true),
                Instr::False => push_bool(&mut self.stack, false),
                Instr::Add => arithmetic!(Op::Add),
                Instr::Sub => arithmetic!(Op::Sub),
                Instr::Mul => arithmetic!(Op::Mul),
                Instr::Div => arithmetic!(Op::Div),
                Instr::Mod => arithmetic!(Op::Mod),
                Instr::Eq => {
                    let (a, b) = operands!();
                    if let Err(error) = equality_operands(a, b) {
                        refuse!(error);
                    }
                    let holds = *a == *b;
                    put(a, Value::Bool(holds));
                    pop_done(&mut self.stack);
                }
                Instr::Neq => {
                    let (a, b) = operands!();
                    if let Err(error) = equality_operands(a, b) {
                        refuse!(error);
                    }
                    let holds = *a != *b;
                    put(a, Value::Bool(holds));
                    pop_done(&mut self.stack);
                }
                Instr::Lt => compare!(Ordering::is_lt),
                Instr::Gt => compare!(Ordering::is_gt),
                Instr::Leq => compare!(Ordering::is_le),
                Instr::Geq => compare!(Ordering::is_ge),
                // An instruction of one operand writes its result over
                // the operand. One that refuses its operand takes it
                // off the stack first.
                Instr::Negate => {
                    let top = self.stack.len() - 1;
                    match sort(&self.stack[top]) {
                        Sort::Int(n) => match n.checked_neg() {
                            Some(v) => put(&mut self.stack[top], Value::Int(v)),
                            None => {
                                pop_done(&mut self.stack);
                                fail!(VmError::new(format!("integer overflow: negate {n}")));
                            }
                        },
                        Sort::Float(n) => put(&mut self.stack[top], Value::Float(-n)),
                        Sort::Bool(_) | Sort::Unit | Sort::Closure(_) | Sort::Other(_) => {
                            let value = pop(&mut self.stack);
                            fail!(VmError::type_confusion(format!(
                                "cannot negate {}",
                                self.user_facing_type_name(&value)
                            )));
                        }
                    }
                }
                Instr::Not => {
                    let top = self.stack.len() - 1;
                    match sort(&self.stack[top]) {
                        Sort::Bool(b) => put(&mut self.stack[top], Value::Bool(!b)),
                        Sort::Int(_)
                        | Sort::Float(_)
                        | Sort::Unit
                        | Sort::Closure(_)
                        | Sort::Other(_) => {
                            let value = pop(&mut self.stack);
                            fail!(VmError::type_confusion(format!(
                                "cannot apply 'not' to {}",
                                self.user_facing_type_name(&value)
                            )));
                        }
                    }
                }
                // A String, an Int, a Float or a Bool stays as it is:
                // `Interpolate` writes it where it goes (`write_plain`,
                // src/vm/show.rs). Any other value is made its text.
                // (Put back, not left with a `continue`: one cost `fib`
                // a fifth more instructions, measured 2026-10-16.)
                Instr::DisplayValue => {
                    let val = pop(&mut self.stack);
                    match &val {
                        Value::String(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_) => {
                            self.stack.push(val)
                        }
                        // Mirror the typechecker's string-interpolation Display
                        // gate (inference.rs ~2654): the typechecker's
                        // `type_name_for_impl` reduces a *concrete* operand to a
                        // canonical name and rejects interpolation when that name
                        // is absent from the Display `trait_impl_set` — which
                        // excludes every first-class value that has no Display
                        // impl: function-shaped values (`Fn`), `Channel`,
                        // `Handle` (task handles), `TcpListener` / `TcpStream`
                        // (opaque resources left explicitly unprintable, see
                        // mod.rs ~7878), and the descriptor values
                        // (`TypeDescriptor` / `PrimitiveDescriptor`). For a
                        // polymorphic type variable the operand type is still
                        // `Var` at the interpolation site (type_name_for_impl ->
                        // None), so the compile-time gate is skipped and the value
                        // reaches here. Erroring at the execution site closes the
                        // silent-wrong-behavior hole and matches how a polymorphic
                        // `x > y` on incomparable values (e.g. two Channels) errors
                        // at runtime rather than silently producing a result.
                        //
                        // The rejected set is sourced from the single predicate
                        // `value_implements_display` (below) so the runtime gate
                        // and the surface-name reporting cannot drift from
                        // `Value::kind`. Parity is locked by
                        // tests/typecheck/round95_interp_display_runtime_tests.rs.
                        _ if !Self::value_implements_display(&val) => {
                            // Report the canonical surface name so the runtime
                            // message matches the typechecker's compile-time one
                            // (which names `Type::Fun` -> "Fn", not the per-shape
                            // `BuiltinFn` / `VariantConstructor` tags). Function-
                            // shaped values, Channel, Handle and the Tcp resources
                            // collapse to their canonical name via
                            // `dispatch_type_name`; the descriptor values
                            // (whose canonical name is the *carried* type name)
                            // fall back to their kind so the diagnostic
                            // names the descriptor kind, not the reflected type.
                            let name = match &val {
                                Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_) => {
                                    val.kind().to_string()
                                }
                                _ => crate::types::canonical::dispatch_type_name(&val),
                            };
                            fail!(VmError::type_confusion(format!(
                                "type '{name}' does not implement Display \
                                 (required for string interpolation)"
                            )));
                        }
                        _ => {
                            save!();
                            let step = ok!(self.shown(&val));
                            go_on!(ok!(self.enter_step(step)));
                        }
                    }
                }
                Instr::Interpolate { segments } => {
                    let written =
                        interpolate(&mut self.stack, &mut self.text, segments, code, chunk);
                    if let Err(at) = written {
                        fail!(VmError::type_confusion(format!(
                            "string interpolation requires string values, got {} \
                             — call `.to_string()` or `.display()` on the value first",
                            self.user_facing_type_name(&self.stack[at])
                        )));
                    }
                }
                Instr::GetLocal { slot } => push_copy!(self.stack, self.stack[base + slot]),
                Instr::SetLocal { slot } => {
                    let top = self.stack.len() - 1;
                    set_copy!(self.stack, base + slot, self.stack[top]);
                }
                Instr::GetGlobal { slot } => match self.view.global(slot) {
                    // (A closure: the VM's own copy of it.)
                    Some(Value::VmClosure(shared)) => {
                        let own = self.own.of(slot, shared);
                        push_closure(&mut self.stack, own);
                    }
                    Some(value) => push_copy!(self.stack, *value),
                    // (Not set in this VM's slots: see the function.)
                    None => {
                        let newest = &self.runtime.newest;
                        match (self.view).global_of_newest(&mut self.view_count, newest, slot) {
                            Ok(value) => {
                                let value = self.own.value(slot, value);
                                self.stack.push(value);
                            }
                            Err(error) => fail!(error),
                        }
                    }
                },
                Instr::SetGlobal { slot } => {
                    let value = peek(&self.stack).clone();
                    save!();
                    if !self.set_global(slot, value) {
                        fail!(VmError::type_confusion(format!(
                            "the global slot {slot} is none of the program's"
                        )));
                    }
                    load!();
                }
                Instr::GetUpvalue { index } => push_copy!(self.stack, closure.upvalues[index]),
                Instr::Call { argc } => {
                    let func_slot = self.stack.len() - 1 - argc;
                    save!();
                    // (A closure is called by its pointer: the value in
                    // the slot is not cloned and moved.)
                    let entered = match sort(&self.stack[func_slot]) {
                        Sort::Closure(closure) => {
                            let closure = closure.clone();
                            ok!(self.call_closure(closure, argc, func_slot))
                        }
                        Sort::Int(_)
                        | Sort::Float(_)
                        | Sort::Bool(_)
                        | Sort::Unit
                        | Sort::Other(_) => {
                            let callee = self.stack[func_slot].clone();
                            ok!(self.call_value(callee, argc, func_slot))
                        }
                    };
                    go_on!(self.entered(entered));
                }
                Instr::TailCall { argc } => {
                    let func_slot = self.stack.len() - 1 - argc;
                    let callee = self.stack[func_slot].clone();
                    save!();
                    if let Value::VmClosure(callee) = callee {
                        if argc != callee.function.arity() {
                            fail!(VmError::type_confusion(format!(
                                "function '{}' expects {} arguments, got {}",
                                callee.function.name(),
                                callee.function.arity(),
                                argc
                            )));
                        }
                        self.reuse_frame(callee, func_slot + 1, argc);
                        load!();
                    } else {
                        let entered = ok!(self.call_value(callee, argc, func_slot));
                        go_on!(self.entered(entered));
                    }
                }
                Instr::Return => {
                    let frames = self.frames.len();
                    let top = self.stack.len() - 1;
                    // Whether the value goes to a function's frame, the
                    // caller's: `returned!` then pushes it where the
                    // callee's own slot was.
                    let to_caller = base >= 1
                        && frames >= 2
                        && frames - 1 != floor
                        && matches!(self.frames[frames - 2], Frame::Code(_));
                    match sort(&self.stack[top]) {
                        // A number, a Bool or unit for the caller is
                        // written from the top slot over the callee's
                        // own: it is not taken off the stack and pushed
                        // again. What is let go of, and in which order,
                        // is `returned!`'s: the frame, the callee's
                        // slot, the slots above it.
                        Sort::Int(_) | Sort::Float(_) | Sort::Bool(_) | Sort::Unit if to_caller => {
                            self.frames.pop();
                            if !self.tco_elided.is_empty() {
                                self.tco_elided.prune(self.frames.len());
                            }
                            set_copy!(self.stack, base - 1, self.stack[top]);
                            self.stack.truncate(base);
                            load!();
                        }
                        Sort::Int(_)
                        | Sort::Float(_)
                        | Sort::Bool(_)
                        | Sort::Unit
                        | Sort::Closure(_)
                        | Sort::Other(_) => returned!(pop(&mut self.stack)),
                    }
                }
                Instr::CallBuiltin { builtin, argc } => {
                    save!();
                    let args_at = self.stack.len() - argc;
                    let entered =
                        ok!(self.with_args(args_at, |vm, args| vm.enter_builtin(builtin, args)));
                    go_on!(self.entered(entered));
                }
                Instr::MakeClosure { f, captures } => {
                    let upvalues = captures
                        .iter(code)
                        .map(|capture| {
                            let index = capture.index;
                            match capture.is_local {
                                true => self.stack[base + index].clone(),
                                false => closure.upvalues[index].clone(),
                            }
                        })
                        .collect();
                    let function = chunk.closure(f).function.clone();
                    self.stack
                        .push(Value::VmClosure(Arc::new(VmClosure { function, upvalues })));
                }
                Instr::MakeTuple { count } => {
                    let start = self.stack.len() - count;
                    let elements = self.stack.drain(start..).collect();
                    self.stack.push(Value::Tuple(elements));
                }
                Instr::MakeList { count } => {
                    let start = self.stack.len() - count;
                    let elements = self.stack.drain(start..).collect();
                    self.stack.push(Value::List(elements));
                }
                Instr::MakeMap { pairs } => {
                    let total = pairs * 2;
                    let start = self.stack.len() - total;
                    let mut map = BTreeMap::new();
                    for i in (start..self.stack.len()).step_by(2) {
                        map.insert(self.stack[i].clone(), self.stack[i + 1].clone());
                    }
                    self.stack.truncate(start);
                    self.stack.push(Value::map(map));
                }
                Instr::MakeSet { count } => {
                    let start = self.stack.len() - count;
                    let mut set = BTreeSet::new();
                    for i in start..self.stack.len() {
                        set.insert(self.stack[i].clone());
                    }
                    self.stack.truncate(start);
                    self.stack.push(Value::set(set));
                }
                Instr::MakeRecord { ty, fields } => {
                    let start = self.stack.len() - fields.len();
                    let values: Vec<Value> = self.stack.drain(start..).collect();
                    let ty = chunk.type_info(ty).clone();
                    let names = fields.iter(code).map(|k| chunk.string(k));
                    let record = ok!(Record::written(ty, names, values).ok_or_else(|| {
                        VmError::type_confusion("a record literal's fields are not its type's")
                    }));
                    self.stack.push(Value::Record(record));
                }
                Instr::RecordUpdate { fields } => {
                    // Functional record update, `r.{ x: 1 }`, and the
                    // fields written after a spread, `{...r, x: 1}`, whose
                    // base is the anonymous record `DestructRecordRest`
                    // made of `r`: see `Record::updated`.
                    let start = self.stack.len() - fields.len();
                    let values: Vec<Value> = self.stack.drain(start..).collect();
                    let base = pop(&mut self.stack);
                    let Value::Record(base) = base else {
                        fail!(VmError::type_confusion(format!(
                            "record update `.{{...}}` requires a record, got {}",
                            self.user_facing_type_name(&base)
                        )));
                    };
                    let names = fields.iter(code).map(|k| chunk.string(k));
                    let updated = ok!(base.updated(names, values).ok_or_else(|| {
                        VmError::type_confusion(
                            "record update `.{...}` of a field the record has not",
                        )
                    }));
                    self.stack.push(Value::Record(updated));
                }
                Instr::MakeRange => {
                    let end = pop(&mut self.stack);
                    let start = pop(&mut self.stack);
                    if let (Value::Int(a), Value::Int(b)) = (&start, &end) {
                        let ints = ok!(List::ints(*a, *b).map_err(VmError::from));
                        self.stack.push(Value::List(ints));
                    } else {
                        fail!(VmError::type_confusion(format!(
                            "range `a..b` requires two Int operands, got {} and {}",
                            self.user_facing_type_name(&start),
                            self.user_facing_type_name(&end)
                        )));
                    }
                }
                Instr::ListConcat => {
                    let b = pop(&mut self.stack);
                    let a = pop(&mut self.stack);
                    let (Value::List(a), Value::List(b)) = (a, b) else {
                        fail!(VmError::type_confusion(
                            "ListConcat: an operand is not a list",
                        ));
                    };
                    let list = ok!(crate::builtins::collections::concat(&a, &b));
                    self.stack.push(list);
                }
                // A read of a field writes the field over the record,
                // on top of the stack: the record is not taken off and
                // the field pushed. A read that fails takes the record
                // off first.
                Instr::GetField { index } => {
                    let top = self.stack.len() - 1;
                    let field = match &self.stack[top] {
                        Value::Record(record) => record.fields().get(index),
                        _ => None,
                    };
                    match field {
                        Some(field) => set_copy!(self.stack, top, *field),
                        None => {
                            let target = pop(&mut self.stack);
                            fail!(VmError::type_confusion(format!(
                                "cannot access field {index} of {}",
                                self.user_facing_type_name(&target)
                            )));
                        }
                    }
                }
                Instr::GetFieldNamed { name } => {
                    let top = self.stack.len() - 1;
                    let name = chunk.string(name);
                    let field = match &self.stack[top] {
                        Value::Record(record) => record.get(name),
                        _ => None,
                    };
                    match field {
                        Some(field) => set_copy!(self.stack, top, *field),
                        None => {
                            let target = pop(&mut self.stack);
                            fail!(match &target {
                                Value::Record(_) =>
                                    VmError::type_confusion(format!("record has no field '{name}'")),
                                other => VmError::type_confusion(format!(
                                    "cannot access field '{}' on {}",
                                    name,
                                    self.user_facing_type_name(other)
                                )),
                            });
                        }
                    }
                }
                Instr::Jump { to } => ip = to,
                Instr::JumpIfFalse { to } => {
                    if self.is_falsy(peek(&self.stack)) {
                        ip = to;
                    }
                    pop_done(&mut self.stack);
                }
                Instr::JumpIfTrue { to } => {
                    if self.is_truthy(peek(&self.stack)) {
                        ip = to;
                    }
                    pop_done(&mut self.stack);
                }
                Instr::Pop => pop_done(&mut self.stack),
                Instr::Dup => {
                    let top = self.stack.len() - 1;
                    push_copy!(self.stack, self.stack[top]);
                }
                Instr::TestTag { tag } => {
                    let expected = chunk.tag(tag);
                    let result = matches!(peek(&self.stack), Value::Variant(variant) if variant.has_tag(expected));
                    push_bool(&mut self.stack, result);
                }
                Instr::TestEqual { k } => {
                    let result = peek(&self.stack) == chunk.constant(k);
                    push_bool(&mut self.stack, result);
                }
                Instr::TestTupleLen { len } => {
                    let val = peek(&self.stack);
                    let result = matches!(val, Value::Tuple(elems) if elems.len() == len);
                    push_bool(&mut self.stack, result);
                }
                Instr::TestListMin { len: min_len } => {
                    let val = peek(&self.stack);
                    let result = matches!(val, Value::List(xs) if xs.len() >= min_len);
                    push_bool(&mut self.stack, result);
                }
                Instr::TestListExact { len } => {
                    let val = peek(&self.stack);
                    let result = matches!(val, Value::List(xs) if xs.len() == len);
                    push_bool(&mut self.stack, result);
                }
                Instr::TestIntRange { lo, hi } => {
                    let result = match (peek(&self.stack), chunk.constant(lo), chunk.constant(hi)) {
                        (Value::Int(n), Value::Int(lo), Value::Int(hi)) => *n >= *lo && *n <= *hi,
                        _ => false,
                    };
                    push_bool(&mut self.stack, result);
                }
                Instr::TestFloatRange { lo, hi } => {
                    let result = match (peek(&self.stack), chunk.constant(lo), chunk.constant(hi)) {
                        (Value::Float(n), Value::Float(lo), Value::Float(hi)) => n >= lo && n <= hi,
                        _ => false,
                    };
                    push_bool(&mut self.stack, result);
                }
                // A pattern's read of a part of the value on top pushes
                // a copy of the part as a read of a local pushes one.
                Instr::DestructTuple { index } => match peek(&self.stack) {
                    Value::Tuple(elems) => match elems.get(index) {
                        Some(elem) => push_copy!(self.stack, *elem),
                        None => fail!(VmError::type_confusion(format!(
                            "tuple destructure: expected at least {} elements, got {}",
                            index + 1,
                            elems.len()
                        ))),
                    },
                    other => fail!(VmError::type_confusion(format!(
                        "tuple destructure: expected tuple, got {}",
                        self.user_facing_type_name(other)
                    ))),
                },
                Instr::DestructVariant { index } => match peek(&self.stack) {
                    Value::Variant(variant) => match variant.fields().get(index) {
                        Some(field) => push_copy!(self.stack, *field),
                        None => fail!(VmError::type_confusion(format!(
                            "variant destructure: field index {} out of bounds (variant has {} fields)",
                            index,
                            variant.fields().len()
                        ))),
                    },
                    other => fail!(VmError::type_confusion(format!(
                        "variant destructure: expected variant, got {}",
                        self.user_facing_type_name(other)
                    ))),
                },
                Instr::DestructList { index } => {
                    let element = match peek(&self.stack) {
                        Value::List(xs) => ok!(xs.get(index).ok_or_else(|| {
                            VmError::type_confusion(format!(
                                "list destructure: expected at least {} elements, got {}",
                                index + 1,
                                xs.len()
                            ))
                        })),
                        other => fail!(VmError::type_confusion(format!(
                            "list destructure: expected list, got {}",
                            self.user_facing_type_name(other)
                        ))),
                    };
                    self.stack.push(element);
                }
                Instr::DestructListRest { start } => {
                    let rest = match peek(&self.stack) {
                        Value::List(xs) if start <= xs.len() => xs.slice(start, xs.len()),
                        Value::List(xs) => fail!(VmError::type_confusion(format!(
                            "list destructure: rest pattern start {} exceeds list length {}",
                            start,
                            xs.len()
                        ))),
                        other => fail!(VmError::type_confusion(format!(
                            "list destructure: expected list, got {}",
                            self.user_facing_type_name(other)
                        ))),
                    };
                    self.stack.push(Value::List(rest));
                }
                Instr::DestructRecordField { index } => {
                    let field = match peek(&self.stack) {
                        Value::Record(record) => record.fields().get(index),
                        _ => None,
                    };
                    match field {
                        Some(field) => push_copy!(self.stack, *field),
                        None => fail!(VmError::type_confusion(format!(
                            "record destructure: no field {index} in {}",
                            self.user_facing_type_name(peek(&self.stack))
                        ))),
                    }
                }
                Instr::DestructRecordFieldNamed { name } => {
                    let name = chunk.string(name);
                    match peek(&self.stack) {
                        Value::Record(record) => match record.get(name) {
                            Some(field) => push_copy!(self.stack, *field),
                            None => fail!(VmError::type_confusion(format!(
                                "record has no field '{name}'"
                            ))),
                        },
                        other => fail!(VmError::type_confusion(format!(
                            "record destructure: expected record, got {}",
                            self.user_facing_type_name(other)
                        ))),
                    }
                }
                Instr::DestructRecordRest { excluded } => {
                    let val = pop(&mut self.stack);
                    let Value::Record(record) = &val else {
                        fail!(VmError::type_confusion(format!(
                            "record rest destructure: expected record, got {}",
                            self.user_facing_type_name(&val)
                        )));
                    };
                    let rest = record.rest(excluded.iter(code).map(|k| chunk.string(k)));
                    self.stack.push(Value::Record(rest));
                }
                Instr::TestRecordTag { ty } => {
                    let expected = chunk.type_info(ty).id;
                    let result = matches!(peek(&self.stack), Value::Record(record) if record.type_id() == expected);
                    push_bool(&mut self.stack, result);
                }
                Instr::TestMapHasKey { key } => {
                    // (The key is the constant itself, a String.)
                    let result = match peek(&self.stack) {
                        Value::Map(map) => map.contains_key(chunk.constant(key)),
                        _ => false,
                    };
                    push_bool(&mut self.stack, result);
                }
                Instr::DestructMapValue { key } => {
                    // (The key is the constant itself, a String.)
                    match peek(&self.stack) {
                        Value::Map(map) => match map.get(chunk.constant(key)) {
                            Some(value) => push_copy!(self.stack, *value),
                            None => fail!(VmError::type_confusion(format!(
                                "map has no key '{}'",
                                chunk.string(key)
                            ))),
                        },
                        other => fail!(VmError::type_confusion(format!(
                            "map destructure: expected map, got {}",
                            self.user_facing_type_name(other)
                        ))),
                    }
                }
                Instr::Recur {
                    argc: arg_count,
                    first: first_slot,
                } => {
                    let start = self.stack.len() - arg_count;
                    for i in 0..arg_count {
                        set_copy!(self.stack, base + first_slot + i, self.stack[start + i]);
                    }
                    // Truncate all the way back to just after loop bindings.
                    self.stack.truncate(base + first_slot + arg_count);
                }
                // `?` on an `Ok` or a `Some` writes what the variant
                // holds over the variant, as a read of a field does.
                Instr::QuestionMark => {
                    let top = self.stack.len() - 1;
                    match &self.stack[top] {
                        Value::Variant(variant) if variant.is(bv::OK) || variant.is(bv::SOME) => {
                            match variant.fields() {
                                [value] => set_copy!(self.stack, top, *value),
                                _ => put(&mut self.stack[top], Value::Unit),
                            }
                        }
                        Value::Variant(variant) if variant.is(bv::ERR) || variant.is(bv::NONE) => {
                            returned!(pop(&mut self.stack));
                        }
                        Value::Variant(variant) => fail!(VmError::type_confusion(format!(
                            "`?` applies only to Result or Option; got variant `{}`",
                            variant.name()
                        ))),
                        other => fail!(VmError::type_confusion(format!(
                            "`?` applies only to Result or Option; got {}",
                            self.user_facing_type_name(other)
                        ))),
                    }
                }
                Instr::Panic => {
                    let msg = pop(&mut self.stack);
                    save!();
                    let step =
                        ok!(self.show(&msg, |_, text| Err(VmError::new(format!("panic: {text}")))));
                    go_on!(ok!(self.enter_step(step)));
                }
                Instr::CallMethod { method, argc, of } => {
                    save!();
                    go_on!(ok!(self.call_method(method, argc, of, false)));
                }
                Instr::TailCallMethod { method, argc, of } => {
                    save!();
                    go_on!(ok!(self.call_method(method, argc, of, true)));
                }
                Instr::Slide { slot } => {
                    // Keep the top value and cut the frame back to
                    // `slot` values below it: the value is written
                    // over the lowest of the values that go (one that
                    // owns something is moved there), not taken off
                    // the stack and pushed again. The values that go
                    // end from the lowest up.
                    let (to, top) = (base + slot, self.stack.len() - 1);
                    if to < top {
                        match sort(&self.stack[top]) {
                            Sort::Int(n) => put(&mut self.stack[to], Value::Int(n)),
                            Sort::Float(f) => put(&mut self.stack[to], Value::Float(f)),
                            Sort::Bool(b) => put(&mut self.stack[to], Value::Bool(b)),
                            Sort::Unit => put(&mut self.stack[to], Value::Unit),
                            Sort::Closure(_) | Sort::Other(_) => {
                                let value = std::mem::replace(&mut self.stack[top], Value::Unit);
                                put(&mut self.stack[to], value);
                            }
                        }
                        self.stack.truncate(to + 1);
                    }
                }
            }
        }
    }

    /// Give `value` to the frame on top: a function's frame gets it on
    /// the stack, and the instruction loop goes on (`None`); a
    /// builtin's frame is resumed with it, until one calls a function
    /// (`None`) or the run ends.
    #[inline(always)]
    fn deliver(
        &mut self,
        floor: usize,
        value: Value,
        budget: &mut usize,
    ) -> Result<Option<Slice>, VmError> {
        // (Where a function's return comes to, as a rule: no call.)
        if self.frames.len() == floor {
            return Ok(Some(Slice::Done(value)));
        }
        if let Some(Frame::Code(_)) = self.frames.last() {
            self.push(value);
            return Ok(None);
        }
        self.deliver_native(floor, value, budget)
    }

    /// [`Vm::deliver`] to a builtin's frame, and to whatever frame is on
    /// top when that one is done.
    fn deliver_native(
        &mut self,
        floor: usize,
        mut value: Value,
        budget: &mut usize,
    ) -> Result<Option<Slice>, VmError> {
        loop {
            if self.frames.len() == floor {
                return Ok(Some(Slice::Done(value)));
            }
            if let Some(Frame::Code(_)) = self.frames.last() {
                self.push(value);
                return Ok(None);
            }
            if *budget == 0 {
                self.pending_input = Some(value);
                return Ok(Some(Slice::OutOfBudget));
            }
            *budget -= 1;
            // The frame is off the list while it runs: it is handed the
            // VM.
            let Some(Frame::Native(mut native)) = self.frames.pop() else {
                unreachable!("the frame on top is a builtin's")
            };
            let step = super::dispatch::resume_native(self, native.as_mut(), value);
            let entered = match step {
                Ok(Step::Done(result)) => {
                    self.native_frames -= 1;
                    Entered::Value(result)
                }
                Ok(Step::Run(next)) => {
                    self.frames.push(Frame::Native(next));
                    Entered::Native
                }
                Ok(Step::Park(wait)) => {
                    self.frames.push(Frame::Native(native));
                    // A VM that waits holds no list for the arguments of
                    // its next builtin: a program can have a hundred
                    // thousand tasks waiting.
                    self.builtin_args = Vec::new();
                    return Ok(Some(Slice::Parked(wait)));
                }
                Ok(Step::Yield) => {
                    self.frames.push(Frame::Native(native));
                    return Ok(Some(Slice::Yielded));
                }
                Ok(Step::Call { callee, argc }) => {
                    self.frames.push(Frame::Native(native));
                    let func_slot = self.stack.len() - argc - 1;
                    self.call_value(callee, argc, func_slot)?
                }
                Err(e) => {
                    self.frames.push(Frame::Native(native));
                    return Err(e);
                }
            };
            value = match entered {
                Entered::Value(result) => result,
                Entered::Native => Value::Unit,
                Entered::Code => return Ok(None),
            };
        }
    }

    /// Drop the frames above the first `floor` and the stack above
    /// `stack_floor`, after an error.
    pub(super) fn unwind(&mut self, floor: usize, stack_floor: usize) {
        while self.frames.len() > floor {
            if let Some(Frame::Native(mut native)) = self.frames.pop() {
                self.native_frames -= 1;
                native.abandon(self);
            }
        }
        self.stack.truncate(stack_floor);
        self.tco_elided.prune(floor);
        self.pending_input = None;
    }

    /// Run the frames above the first `floor` to their end on the
    /// calling thread, starting with `start`. The thread counts as a
    /// task of the scheduler while it does; where the code parks, the
    /// thread waits, and the builtin that parked runs again when its
    /// wait has ended.
    ///
    /// If a thread of the runtime has panicked, the program's own
    /// thread gives the panic on from here, before a slice and where
    /// it waits (`Scheduler::give_panic_on`): the program has ended.
    pub(super) fn run_thread(
        &mut self,
        floor: usize,
        start: impl FnOnce(&mut Vm) -> Result<Slice, VmError>,
    ) -> Result<Slice, VmError> {
        let scheduler = self.runtime.scheduler.clone();
        let _running = scheduler.enter();
        let program = self.is_program();
        if program {
            scheduler.give_panic_on();
        }
        // No slice starts once the step budget is used up, as for a
        // task (`execute_slice`).
        self.runtime.charge(0)?;
        let mut run = start(self)?;
        loop {
            if program {
                scheduler.give_panic_on();
            }
            match run {
                Slice::Done(_) => return Ok(run),
                // The frame gave way to other tasks, which a thread
                // of its own has no need to.
                Slice::OutOfBudget => self.runtime.charge(self.own_slice())?,
                // A slice given away is a step of the budget, like a
                // wait: it did not run its full length.
                Slice::Yielded => {
                    let _ = self.runtime.charge(1);
                }
                Slice::Parked(wait) => {
                    // A wait is a step of the budget (the error comes
                    // before the next slice).
                    let _ = self.runtime.charge(1);
                    let who = match program {
                        true => Blocks::Program,
                        false => Blocks::Thread,
                    };
                    // A wait that cannot end (a deadlock) after the step
                    // budget is used up is the budget's doing: the task
                    // waited for was ended.
                    let woken = scheduler.block_thread(wait, who).map_err(|mut e| {
                        e.out_of_steps |= self.runtime.charge(0).is_err();
                        e
                    })?;
                    self.woken = Some(woken);
                    #[cfg(any(test, feature = "test-hooks"))]
                    if program {
                        scheduler.panic_of_the_program_for_test();
                    }
                }
            }
            // A thread whose slices all end in a wait never runs one
            // out: the budget its waits have used up ends it here.
            if let Err(e) = self.runtime.charge(0) {
                self.woken = None;
                return Err(e);
            }
            run = self.run_frames(floor, self.own_slice())?;
        }
    }

    /// The value of a run to the end, or its error with the call stack
    /// of the frames it leaves, which are dropped.
    pub(super) fn finish_run(
        &mut self,
        run: Result<Slice, VmError>,
        floor: usize,
        stack_floor: usize,
    ) -> Result<Value, VmError> {
        let error = match run {
            Ok(Slice::Done(value)) => return Ok(value),
            Ok(Slice::OutOfBudget | Slice::Yielded | Slice::Parked(_)) => {
                VmError::new("internal VM error: a run to the end stopped before it".into())
            }
            Err(e) => e,
        };
        // The call stack is read off the frames this run leaves, which
        // must not stay: the next run on this VM (a REPL's next entry,
        // a stage's next item) would show them as its own.
        let enriched = self.enrich_error(error);
        self.unwind(floor, stack_floor);
        Err(enriched)
    }

    // ── Sliced execution (for M:N scheduler) ─────────────────────

    /// Run a task's frames for up to `max_steps` steps and return a
    /// `SliceResult`. Used by the M:N scheduler's worker threads.
    pub(crate) fn execute_slice(&mut self, max_steps: usize) -> SliceResult {
        // A task of a program whose step budget is used up runs no
        // further.
        if let Err(e) = self.runtime.charge(0) {
            return SliceResult::Failed(e);
        }
        match self.run_frames(0, max_steps) {
            Ok(Slice::Done(value)) => SliceResult::Completed(value),
            Ok(Slice::OutOfBudget) => match self.runtime.charge(max_steps) {
                Ok(()) => SliceResult::Yielded,
                Err(e) => SliceResult::Failed(e),
            },
            Ok(Slice::Yielded) => {
                // A slice given away is a step of the budget.
                let _ = self.runtime.charge(1);
                SliceResult::Yielded
            }
            Ok(Slice::Parked(wait)) => {
                // A wait is a step of the budget: the task ends where
                // its next slice would start.
                let _ = self.runtime.charge(1);
                SliceResult::Blocked(wait)
            }
            Err(e) => SliceResult::Failed(e),
        }
    }

    /// What the loop does after a call an instruction made: a value is
    /// the instruction's result.
    fn entered(&mut self, entered: Entered) -> DispatchResult {
        match entered {
            Entered::Value(value) => {
                self.push(value);
                DispatchResult::Continue
            }
            Entered::Code => DispatchResult::Continue,
            Entered::Native => DispatchResult::Native,
        }
    }

    /// Go on as `step`, what showing a value came to: its value is the
    /// instruction's, at once or when its frame is done.
    fn enter_step(&mut self, step: Step) -> Result<DispatchResult, VmError> {
        match step {
            Step::Done(value) => {
                self.push(value);
                Ok(DispatchResult::Continue)
            }
            Step::Run(native) => {
                self.push_native_frame(native);
                Ok(DispatchResult::Native)
            }
            Step::Call { .. } | Step::Park(_) | Step::Yield => Err(VmError::new(
                "internal VM error: a shown value calls or waits without a frame".into(),
            )),
        }
    }

    /// Run `closure` in the current frame, with the `argc` values from
    /// the stack's slot `args_at` on as its arguments: a tail call.
    #[inline(always)]
    fn reuse_frame(&mut self, closure: Arc<VmClosure>, args_at: usize, argc: usize) {
        let base = self.frame().base_slot;
        for i in 0..argc {
            self.stack[base + i] = self.stack[args_at + i].clone();
        }
        self.stack.truncate(base + argc);
        // The caller, which the callee takes the place of, goes to the
        // log, for the call stack of an error: its function and where
        // it had come to, which is past the tail call. (No name and no
        // place are looked up here: `Vm::enrich_error` does that.)
        let depth = self.frames.len().saturating_sub(1);
        let frame = self.frame_mut();
        let caller = std::mem::replace(&mut frame.closure, closure);
        let caller_ip = std::mem::replace(&mut frame.ip, 0);
        self.tco_elided
            .log(depth, caller.function.clone(), caller_ip);
    }

    /// The global slot of the method of the impl of the trait
    /// `trait_index` names for the type `ty`, whose name has the id
    /// `method`, in this VM's table, if the slot is set.
    #[inline(always)]
    fn method_of(&self, trait_index: u16, ty: crate::defs::TypeId, method: u16) -> Option<u16> {
        let slot = (self.view.table).call_method(trait_index, ty, method)?;
        self.global(slot).map(|_| slot)
    }

    /// [`Vm::method_of`] found none: the method in the program's newest
    /// table, and in the table of the REPL entry that is being run,
    /// which have a later entry's impls for the VM of a task of an
    /// earlier one (see [`Vm::look_again`]).
    #[cold]
    #[inline(never)]
    fn method_of_newest(
        &mut self,
        trait_index: u16,
        ty: crate::defs::TypeId,
        method: u16,
    ) -> Option<u16> {
        if self.look_again()
            && let Some(found) = self.method_of(trait_index, ty, method)
        {
            return Some(found);
        }
        if Arc::ptr_eq(&self.view.table, &self.view.entry) {
            return None;
        }
        let slot = (self.view.entry).call_method(trait_index, ty, method)?;
        self.global(slot).map(|_| slot)
    }

    /// `CallMethod`, and `TailCallMethod` (`tail`): call the method
    /// whose name has the id `method`, of the trait `trait_index`
    /// names, for the type of the receiver, the first of the top
    /// `argc` values.
    fn call_method(
        &mut self,
        method: u16,
        argc: usize,
        trait_index: u16,
        tail: bool,
    ) -> Result<DispatchResult, VmError> {
        let receiver_slot = self.stack.len() - argc;
        // (The receiver is looked at where it is: a method of an impl
        // is found by its type alone.)
        let receiver = &self.stack[receiver_slot];
        let receiver_type = crate::types::canonical::dispatch_type_for_value(receiver);
        // Descriptor-as-receiver (e.g. `Int.default()`,
        // `body.decode(Todo)` where the descriptor is piped in) is
        // a dispatch key, not a value argument. The method's
        // compiled body never has a slot for it — skip it when
        // assembling the argument vector.
        let descriptor_receiver = matches!(
            receiver,
            Value::TypeDescriptor(_) | Value::PrimitiveDescriptor(_)
        );
        // The method of an impl is found by three numbers: the trait,
        // the receiver's type, and the name's id. No text is compared.
        let found = match self.method_of(trait_index, receiver_type, method) {
            None => self.method_of_newest(trait_index, receiver_type, method),
            found => found,
        };
        // (A method that is a closure, which a written method is, is
        // taken by its pointer, as `Call` takes one: the VM's own copy
        // of the closure in the global's slot is not cloned and moved.)
        if let Some(slot) = found
            && let Some(func) = self.view.global(slot)
        {
            match sort(func) {
                Sort::Closure(shared) => {
                    let closure = self.own.of(slot, shared);
                    // In tail position the method takes the current
                    // frame over, as a tail-called function does.
                    if tail {
                        let args_at = receiver_slot + usize::from(descriptor_receiver);
                        let argc = self.stack.len() - args_at;
                        if argc != closure.function.arity() {
                            return Err(VmError::type_confusion(format!(
                                "function '{}' expects {} arguments, got {}",
                                closure.function.name(),
                                closure.function.arity(),
                                argc
                            )));
                        }
                        self.reuse_frame(closure, args_at, argc);
                        return Ok(DispatchResult::Continue);
                    }
                    // The method's frame starts above a slot of its
                    // own, as a called function's does: the
                    // descriptor's, or one made below the receiver.
                    if !descriptor_receiver {
                        open_slot(&mut self.stack, receiver_slot);
                    }
                    let argc = self.stack.len() - receiver_slot - 1;
                    let entered = self.call_closure(closure, argc, receiver_slot)?;
                    return Ok(self.entered(entered));
                }
                Sort::Int(_) | Sort::Float(_) | Sort::Bool(_) | Sort::Unit | Sort::Other(_) => {
                    let func = func.clone();
                    if !descriptor_receiver {
                        open_slot(&mut self.stack, receiver_slot);
                    }
                    let argc = self.stack.len() - receiver_slot - 1;
                    let entered = self.call_value(func, argc, receiver_slot)?;
                    return Ok(self.entered(entered));
                }
            }
        }
        // No impl has the method for the receiver's type: what the
        // call does then goes by the method's name.
        let receiver = self.stack[receiver_slot].clone();
        let slots = self.view.entry.clone();
        // (The compiler checks each function against the globals of
        // the program it is compiled into, and the VM of a task that
        // runs code of a later REPL entry has that entry's by now: the
        // call found no impl, so the VM has looked for the newest.)
        let Some(method_name) = slots.method_name(method) else {
            return Err(VmError::type_confusion(
                "the call of a method names a method the program does not have",
            ));
        };
        if usize::from(trait_index) >= slots.trait_count() {
            return Err(VmError::type_confusion(format!(
                "the call of method '{method_name}' names a trait the program does not have"
            )));
        }
        // `display` of a value whose parts a written impl may show.
        if method_name == "display"
            && argc == 1
            && slots.any_shown()
            && Self::value_implements_display(&receiver)
        {
            self.stack.truncate(receiver_slot);
            let step = self.shown(&receiver)?;
            return self.enter_step(step);
        }
        // `message` of a builtin error enum (`IoError`, ...): the row
        // of the enum, found by the type's id, called as any builtin.
        if method_name == "message"
            && let Value::Variant(variant) = &receiver
            && let Some(module) = registry().error_module(variant.ty().id)
            && let Some(row) = module.message.as_ref().filter(|row| row.enabled)
        {
            let entered = self.with_args(receiver_slot, |vm, args| vm.enter_row(row, args))?;
            return Ok(self.entered(entered));
        }
        // A builtin trait's method the type has natively (display,
        // equal, compare, hash).
        match self.dispatch_trait_method(&receiver, method_name, &self.stack[receiver_slot + 1..]) {
            Some(result) => {
                self.stack.truncate(receiver_slot);
                self.push(result?);
            }
            // `message` of an `Error` impl that leaves it out is the
            // value's `display`.
            None if method_name == "message"
                && argc == 1
                && slots.trait_at(trait_index) == crate::defs::builtin_trait_id("Error") =>
            {
                self.stack.truncate(receiver_slot);
                let step = self.shown(&receiver)?;
                return self.enter_step(step);
            }
            None => {
                return Err(VmError::type_confusion(format!(
                    "no method '{method_name}' for type '{}'",
                    crate::types::canonical::dispatch_type_name(&receiver)
                )));
            }
        }

        Ok(DispatchResult::Continue)
    }
}
