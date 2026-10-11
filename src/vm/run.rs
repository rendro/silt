//! Main execution loop and opcode dispatch.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::builtins::registry::registry;
use crate::bytecode::{Chunk, Instr, Op, VmClosure, decode};
use crate::scheduler::{Blocks, SliceResult};
use crate::typeinfo::bv;
use crate::value::{List, Record, Value};

use super::arithmetic::{arithmetic, check_same_type, ordering};
use super::calls::Entered;
use super::runtime::{Frame, Step};
use super::{Vm, VmError};
use crate::runtime::sync::Wait;

/// Gate the language-level `==` / `!=` operators against function-shaped
/// values, returning the canonical surface name (`"Fn"`) to name in the
/// error if the value cannot participate in equality, or `None` if it can.
///
/// The checker rejects `==` on a value that is, or holds, a function
/// (`Equal` is decided by structure, for a concrete operand and through
/// a bound alike): this is the backstop at the execution site, where
/// `PartialEq for Value` would answer with `Arc::ptr_eq` on closures and
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

/// A copy of `value`, for the instructions that read a slot or a
/// constant: a number, a Bool or unit is copied here, in the loop; a
/// value of any other kind is cloned, a call. (The kinds that are
/// copied are named: one that is added later is cloned until it is
/// named here.)
#[inline(always)]
fn copy(value: &Value) -> Value {
    match value {
        Value::Int(n) => Value::Int(*n),
        Value::Float(f) => Value::Float(*f),
        Value::Bool(b) => Value::Bool(*b),
        Value::Unit => Value::Unit,
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
        | Value::TcpStream(_) => value.clone(),
    }
}

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
        macro_rules! arithmetic {
            ($op:expr) => {{
                let b = pop(&mut self.stack);
                let a = pop(&mut self.stack);
                let value = ok!(arithmetic($op, &a, &b));
                self.stack.push(value);
            }};
        }
        macro_rules! compare {
            ($holds:expr) => {{
                let b = pop(&mut self.stack);
                let a = pop(&mut self.stack);
                let ordering = ok!(ordering(&a, &b));
                self.stack.push(Value::Bool($holds(ordering)));
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
                Instr::Constant { k } => self.stack.push(copy(chunk.constant(k))),
                Instr::Unit => self.stack.push(Value::Unit),
                Instr::True => self.stack.push(Value::Bool(true)),
                Instr::False => self.stack.push(Value::Bool(false)),
                Instr::Add => arithmetic!(Op::Add),
                Instr::Sub => arithmetic!(Op::Sub),
                Instr::Mul => arithmetic!(Op::Mul),
                Instr::Div => arithmetic!(Op::Div),
                Instr::Mod => arithmetic!(Op::Mod),
                Instr::Eq => {
                    let b = pop(&mut self.stack);
                    let a = pop(&mut self.stack);
                    ok!(equality_operands(&a, &b));
                    self.stack.push(Value::Bool(a == b));
                }
                Instr::Neq => {
                    let b = pop(&mut self.stack);
                    let a = pop(&mut self.stack);
                    ok!(equality_operands(&a, &b));
                    self.stack.push(Value::Bool(a != b));
                }
                Instr::Lt => compare!(Ordering::is_lt),
                Instr::Gt => compare!(Ordering::is_gt),
                Instr::Leq => compare!(Ordering::is_le),
                Instr::Geq => compare!(Ordering::is_ge),
                Instr::Negate => match pop(&mut self.stack) {
                    Value::Int(n) => match n.checked_neg() {
                        Some(v) => self.stack.push(Value::Int(v)),
                        None => fail!(VmError::new(format!("integer overflow: negate {n}"))),
                    },
                    Value::Float(n) => self.stack.push(Value::Float(-n)),
                    other => fail!(VmError::type_confusion(format!(
                        "cannot negate {}",
                        self.user_facing_type_name(&other)
                    ))),
                },
                Instr::Not => match pop(&mut self.stack) {
                    Value::Bool(b) => self.stack.push(Value::Bool(!b)),
                    other => fail!(VmError::type_confusion(format!(
                        "cannot apply 'not' to {}",
                        self.user_facing_type_name(&other)
                    ))),
                },
                Instr::DisplayValue => {
                    let val = pop(&mut self.stack);
                    match &val {
                        Value::String(_) => self.stack.push(val),
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
                Instr::StringConcat { count } => {
                    let start = self.stack.len() - count;
                    // Pre-calculate total capacity to avoid reallocations
                    let mut total_len = 0;
                    for i in start..self.stack.len() {
                        if let Value::String(ref s) = self.stack[i] {
                            total_len += s.len();
                        } else {
                            fail!(VmError::type_confusion(format!(
                                "string interpolation requires string values, got {} \
                                 — call `.to_string()` or `.display()` on the value first",
                                self.user_facing_type_name(&self.stack[i])
                            )));
                        }
                    }
                    let mut result = String::with_capacity(total_len);
                    for i in start..self.stack.len() {
                        if let Value::String(ref s) = self.stack[i] {
                            result.push_str(s);
                        }
                    }
                    self.stack.truncate(start);
                    self.stack.push(Value::String(result.into()));
                }
                Instr::GetLocal { slot } => {
                    let value = copy(&self.stack[base + slot]);
                    self.stack.push(value);
                }
                Instr::SetLocal { slot } => {
                    let value = copy(peek(&self.stack));
                    self.stack[base + slot] = value;
                }
                Instr::GetGlobal { slot } => match self.global(slot) {
                    Some(value) => {
                        let value = value.clone();
                        self.stack.push(value);
                    }
                    // (The checker orders the top-level `let`s so that
                    // each is set before anything can read it, and the
                    // definitions are set before any `let` runs: no
                    // checked program that is run as a whole comes
                    // here. A task of an earlier REPL entry does, when
                    // it runs a closure of a later entry that names
                    // what that entry defined.)
                    None => fail!(VmError::type_confusion(format!(
                        "the global '{}' is read before it is set",
                        self.global_slots.name(slot)
                    ))),
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
                Instr::GetUpvalue { index } => self.stack.push(copy(&closure.upvalues[index])),
                Instr::Call { argc } => {
                    let func_slot = self.stack.len() - 1 - argc;
                    let callee = self.stack[func_slot].clone();
                    save!();
                    let entered = ok!(self.call_value(callee, argc, func_slot));
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
                Instr::Return => returned!(pop(&mut self.stack)),
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
                Instr::GetField { index } => {
                    let target = pop(&mut self.stack);
                    let field = match &target {
                        Value::Record(record) => record.fields().get(index).cloned(),
                        _ => None,
                    };
                    let field = ok!(field.ok_or_else(|| {
                        VmError::type_confusion(format!(
                            "cannot access field {index} of {}",
                            self.user_facing_type_name(&target)
                        ))
                    }));
                    self.stack.push(field);
                }
                Instr::GetFieldNamed { name } => {
                    let target = pop(&mut self.stack);
                    let name = chunk.string(name);
                    let field = match &target {
                        Value::Record(record) => ok!(record.get(name).cloned().ok_or_else(|| {
                            VmError::type_confusion(format!("record has no field '{name}'"))
                        })),
                        other => fail!(VmError::type_confusion(format!(
                            "cannot access field '{}' on {}",
                            name,
                            self.user_facing_type_name(other)
                        ))),
                    };
                    self.stack.push(field);
                }
                Instr::Jump { to } => ip = to,
                Instr::JumpIfFalse { to } => {
                    let val = pop(&mut self.stack);
                    if self.is_falsy(&val) {
                        ip = to;
                    }
                }
                Instr::JumpIfTrue { to } => {
                    let val = pop(&mut self.stack);
                    if self.is_truthy(&val) {
                        ip = to;
                    }
                }
                Instr::Pop => {
                    pop(&mut self.stack);
                }
                Instr::Dup => {
                    let val = peek(&self.stack).clone();
                    self.stack.push(val);
                }
                Instr::TestTag { tag } => {
                    let expected = chunk.tag(tag);
                    let result = matches!(peek(&self.stack), Value::Variant(variant) if variant.has_tag(expected));
                    self.stack.push(Value::Bool(result));
                }
                Instr::TestEqual { k } => {
                    let result = peek(&self.stack) == chunk.constant(k);
                    self.stack.push(Value::Bool(result));
                }
                Instr::TestTupleLen { len } => {
                    let val = peek(&self.stack);
                    let result = matches!(val, Value::Tuple(elems) if elems.len() == len);
                    self.stack.push(Value::Bool(result));
                }
                Instr::TestListMin { len: min_len } => {
                    let val = peek(&self.stack);
                    let result = matches!(val, Value::List(xs) if xs.len() >= min_len);
                    self.stack.push(Value::Bool(result));
                }
                Instr::TestListExact { len } => {
                    let val = peek(&self.stack);
                    let result = matches!(val, Value::List(xs) if xs.len() == len);
                    self.stack.push(Value::Bool(result));
                }
                Instr::TestIntRange { lo, hi } => {
                    let result = match (peek(&self.stack), chunk.constant(lo), chunk.constant(hi)) {
                        (Value::Int(n), Value::Int(lo), Value::Int(hi)) => *n >= *lo && *n <= *hi,
                        _ => false,
                    };
                    self.stack.push(Value::Bool(result));
                }
                Instr::TestFloatRange { lo, hi } => {
                    let result = match (peek(&self.stack), chunk.constant(lo), chunk.constant(hi)) {
                        (Value::Float(n), Value::Float(lo), Value::Float(hi)) => n >= lo && n <= hi,
                        _ => false,
                    };
                    self.stack.push(Value::Bool(result));
                }
                Instr::DestructTuple { index } => {
                    let val = peek(&self.stack).clone();
                    if let Value::Tuple(elems) = val {
                        let elem = ok!(elems.get(index).ok_or_else(|| {
                            VmError::type_confusion(format!(
                                "tuple destructure: expected at least {} elements, got {}",
                                index + 1,
                                elems.len()
                            ))
                        }));
                        self.stack.push(elem.clone());
                    } else {
                        fail!(VmError::type_confusion(format!(
                            "tuple destructure: expected tuple, got {}",
                            self.user_facing_type_name(&val)
                        )));
                    }
                }
                Instr::DestructVariant { index } => {
                    let field = match peek(&self.stack) {
                        Value::Variant(variant) => {
                            let fields = variant.fields();
                            ok!(fields.get(index).cloned().ok_or_else(|| {
                                VmError::type_confusion(format!(
                                    "variant destructure: field index {} out of bounds (variant has {} fields)",
                                    index,
                                    fields.len()
                                ))
                            }))
                        }
                        other => fail!(VmError::type_confusion(format!(
                            "variant destructure: expected variant, got {}",
                            self.user_facing_type_name(other)
                        ))),
                    };
                    self.stack.push(field);
                }
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
                        Value::Record(record) => record.fields().get(index).cloned(),
                        _ => None,
                    };
                    let field = ok!(field.ok_or_else(|| {
                        VmError::type_confusion(format!(
                            "record destructure: no field {index} in {}",
                            self.user_facing_type_name(peek(&self.stack))
                        ))
                    }));
                    self.stack.push(field);
                }
                Instr::DestructRecordFieldNamed { name } => {
                    let name = chunk.string(name);
                    let field = match peek(&self.stack) {
                        Value::Record(record) => ok!(record.get(name).cloned().ok_or_else(|| {
                            VmError::type_confusion(format!("record has no field '{name}'"))
                        })),
                        other => fail!(VmError::type_confusion(format!(
                            "record destructure: expected record, got {}",
                            self.user_facing_type_name(other)
                        ))),
                    };
                    self.stack.push(field);
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
                    self.stack.push(Value::Bool(result));
                }
                Instr::TestMapHasKey { key } => {
                    // (The key is the constant itself, a String.)
                    let result = match peek(&self.stack) {
                        Value::Map(map) => map.contains_key(chunk.constant(key)),
                        _ => false,
                    };
                    self.stack.push(Value::Bool(result));
                }
                Instr::DestructMapValue { key } => {
                    // (The key is the constant itself, a String.)
                    let value = match peek(&self.stack) {
                        Value::Map(map) => {
                            ok!(map.get(chunk.constant(key)).cloned().ok_or_else(|| {
                                VmError::type_confusion(format!(
                                    "map has no key '{}'",
                                    chunk.string(key)
                                ))
                            }))
                        }
                        other => fail!(VmError::type_confusion(format!(
                            "map destructure: expected map, got {}",
                            self.user_facing_type_name(other)
                        ))),
                    };
                    self.stack.push(value);
                }
                Instr::Recur {
                    argc: arg_count,
                    first: first_slot,
                } => {
                    let start = self.stack.len() - arg_count;
                    for i in 0..arg_count {
                        self.stack[base + first_slot + i] = copy(&self.stack[start + i]);
                    }
                    // Truncate all the way back to just after loop bindings.
                    self.stack.truncate(base + first_slot + arg_count);
                }
                Instr::QuestionMark => {
                    let val = peek(&self.stack).clone();
                    match val {
                        Value::Variant(variant) => match variant {
                            _ if variant.is(bv::OK) || variant.is(bv::SOME) => {
                                pop(&mut self.stack);
                                self.stack.push(match variant.fields() {
                                    [value] => value.clone(),
                                    _ => Value::Unit,
                                });
                            }
                            _ if variant.is(bv::ERR) || variant.is(bv::NONE) => {
                                returned!(pop(&mut self.stack));
                            }
                            _ => fail!(VmError::type_confusion(format!(
                                "`?` applies only to Result or Option; got variant `{}`",
                                variant.name()
                            ))),
                        },
                        _ => fail!(VmError::type_confusion(format!(
                            "`?` applies only to Result or Option; got {}",
                            self.user_facing_type_name(&val)
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
                    // Keep the top value, cut the frame back to `slot` values
                    // and put the value on top of them.
                    let value = pop(&mut self.stack);
                    self.stack.truncate(base + slot);
                    self.stack.push(value);
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
    pub(super) fn run_thread(
        &mut self,
        floor: usize,
        start: impl FnOnce(&mut Vm) -> Result<Slice, VmError>,
    ) -> Result<Slice, VmError> {
        let scheduler = self.runtime.scheduler.clone();
        let _running = scheduler.enter();
        // No slice starts once the step budget is used up, as for a
        // task (`execute_slice`).
        self.runtime.charge(0)?;
        let mut run = start(self)?;
        loop {
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
                    let who = match self.is_program() {
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
        let found = self
            .global_slots
            .call_method(trait_index, receiver_type, method)
            .and_then(|slot| self.global(slot).cloned());
        if let Some(func) = found {
            // In tail position a method that is a closure takes the
            // current frame over, as a tail-called function does.
            if tail && let Value::VmClosure(closure) = &func {
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
                self.reuse_frame(closure.clone(), args_at, argc);
                return Ok(DispatchResult::Continue);
            }
            // The method's frame starts above a slot of its own,
            // as a called function's does: the descriptor's, or
            // one made below the receiver.
            if !descriptor_receiver {
                self.stack.insert(receiver_slot, Value::Unit);
            }
            let argc = self.stack.len() - receiver_slot - 1;
            let entered = self.call_value(func, argc, receiver_slot)?;
            return Ok(self.entered(entered));
        }
        // No impl has the method for the receiver's type: what the
        // call does then goes by the method's name.
        let receiver = self.stack[receiver_slot].clone();
        let slots = self.global_slots.clone();
        // (The compiler checks each function against the globals of
        // the program it is compiled into. Code of a later REPL entry
        // that a task of an earlier entry runs is the exception: the
        // task has the globals of its own entry.)
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
            && self.global_slots.any_shown()
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
                && self.global_slots.trait_at(trait_index)
                    == crate::defs::builtin_trait_id("Error") =>
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
