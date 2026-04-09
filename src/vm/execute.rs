//! Main execution loop and opcode dispatch.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::bytecode::{Op, VmClosure};
use crate::scheduler::SliceResult;
use crate::value::{Value, checked_range_len};

use super::runtime::CallFrame;
use super::{Vm, VmError};

/// Result of dispatching a single opcode.
pub(super) enum DispatchResult {
    /// Normal execution; continue to next opcode.
    Continue,
    /// Op::Return was executed. The return value is provided.
    /// The frame has NOT been popped — the caller must do that.
    Return(Value),
    /// Op::QuestionMark hit Err/None. The frame HAS been popped.
    /// The value and the finished frame's base_slot are provided.
    /// The caller must handle stack cleanup.
    EarlyReturn { value: Value, finished_base: usize },
}

impl Vm {
    // ── Main execution loop ───────────────────────────────────────

    pub(crate) fn execute(&mut self) -> Result<Value, VmError> {
        loop {
            let op_byte = self.read_byte()?;
            let op = Op::from_byte(op_byte)
                .ok_or_else(|| VmError::new(format!("unknown opcode: {op_byte}")))?;
            match self.dispatch_one(op)? {
                DispatchResult::Continue => {}
                DispatchResult::Return(result) => {
                    let finished_base = self.current_frame()?.base_slot;
                    self.frames.pop();
                    if self.frames.is_empty() {
                        return Ok(result);
                    }
                    let func_slot = finished_base.saturating_sub(1);
                    self.stack.truncate(func_slot);
                    self.push(result);
                }
                DispatchResult::EarlyReturn {
                    value,
                    finished_base,
                } => {
                    if self.frames.is_empty() {
                        return Ok(value);
                    }
                    let func_slot = finished_base.saturating_sub(1);
                    self.stack.truncate(func_slot);
                    self.push(value);
                }
            }
        }
    }

    // ── Sliced execution (for M:N scheduler) ─────────────────────

    /// Run up to `max_steps` instructions and return a `SliceResult`.
    /// Used by the M:N scheduler's worker threads.
    pub fn execute_slice(&mut self, max_steps: usize) -> SliceResult {
        // Helper macro to convert Result to SliceResult::Failed on error.
        macro_rules! try_or_fail {
            ($expr:expr) => {
                match $expr {
                    Ok(v) => v,
                    Err(e) => return SliceResult::Failed(e),
                }
            };
        }
        for _ in 0..max_steps {
            if self.frames.is_empty() {
                let result = if self.stack.is_empty() {
                    Value::Unit
                } else {
                    self.stack.last().cloned().unwrap_or(Value::Unit)
                };
                return SliceResult::Completed(result);
            }
            let saved_ip = try_or_fail!(self.current_frame()).ip;
            let op_byte = try_or_fail!(self.read_byte());
            let op = match Op::from_byte(op_byte) {
                Some(op) => op,
                None => {
                    return SliceResult::Failed(VmError::new(format!("unknown opcode: {op_byte}")));
                }
            };
            match self.dispatch_one(op) {
                Ok(DispatchResult::Continue) => {}
                Ok(DispatchResult::Return(result)) => {
                    let finished_base = try_or_fail!(self.current_frame()).base_slot;
                    self.frames.pop();
                    if self.frames.is_empty() {
                        return SliceResult::Completed(result);
                    }
                    let func_slot = finished_base.saturating_sub(1);
                    self.stack.truncate(func_slot);
                    self.push(result);
                }
                Ok(DispatchResult::EarlyReturn {
                    value,
                    finished_base,
                }) => {
                    if self.frames.is_empty() {
                        return SliceResult::Completed(value);
                    }
                    let func_slot = finished_base.saturating_sub(1);
                    self.stack.truncate(func_slot);
                    self.push(value);
                }
                Err(e) if e.is_yield => {
                    try_or_fail!(self.current_frame_mut()).ip = saved_ip;
                    if self.block_reason.is_some() {
                        return SliceResult::Blocked;
                    }
                    return SliceResult::Yielded;
                }
                Err(e) => return SliceResult::Failed(e),
            }
            if self.block_reason.is_some() {
                return SliceResult::Blocked;
            }
        }
        // Time slice expired.
        SliceResult::Yielded
    }

    // ── Call a value ──────────────────────────────────────────────

    pub(super) fn call_value(
        &mut self,
        func_val: Value,
        argc: usize,
        func_slot: usize,
    ) -> Result<(), VmError> {
        const MAX_FRAMES: usize = 100_000;
        match func_val {
            Value::VmClosure(closure) => {
                if argc != closure.function.arity as usize {
                    return Err(VmError::new(format!(
                        "function '{}' expects {} arguments, got {}",
                        closure.function.name, closure.function.arity, argc
                    )));
                }
                if self.frames.len() >= MAX_FRAMES {
                    return Err(VmError::new(format!(
                        "stack overflow: recursion depth exceeded {} frames (tip: put the recursive call in tail position to avoid this limit)",
                        MAX_FRAMES
                    )));
                }
                // Push a new call frame. The arguments are already on the stack
                // at positions [func_slot+1 .. func_slot+1+argc].
                // The base_slot for the new frame is func_slot+1 so the args
                // are at locals[0..argc].
                self.frames.push(CallFrame {
                    closure,
                    ip: 0,
                    base_slot: func_slot + 1,
                });
                Ok(())
            }
            Value::BuiltinFn(name) => {
                let start = func_slot + 1;
                let args: Vec<Value> = self.stack[start..start + argc].to_vec();
                // Pop everything including the function slot
                self.stack.truncate(func_slot);
                let result = self.dispatch_builtin(&name, &args)?;
                self.push(result);
                Ok(())
            }
            Value::VariantConstructor(name, arity) => {
                if argc != arity {
                    return Err(VmError::new(format!(
                        "variant constructor '{name}' expects {arity} arguments, got {argc}"
                    )));
                }
                let start = func_slot + 1;
                let fields: Vec<Value> = self.stack[start..start + argc].to_vec();
                self.stack.truncate(func_slot);
                self.push(Value::Variant(name, fields));
                Ok(())
            }
            _ => Err(VmError::new(format!(
                "cannot call value of type {}",
                self.type_name(&func_val)
            ))),
        }
    }

    /// Call a callable Value and return its result. Used for higher-order builtins.
    pub(crate) fn invoke_callable(
        &mut self,
        func: &Value,
        args: &[Value],
    ) -> Result<Value, VmError> {
        match func {
            Value::VmClosure(closure) => {
                if args.len() != closure.function.arity as usize {
                    return Err(VmError::new(format!(
                        "function '{}' expects {} arguments, got {}",
                        closure.function.name,
                        closure.function.arity,
                        args.len()
                    )));
                }
                const MAX_FRAMES: usize = 100_000;
                if self.frames.len() >= MAX_FRAMES {
                    return Err(VmError::new(format!(
                        "stack overflow: recursion depth exceeded {} frames (tip: put the recursive call in tail position to avoid this limit)",
                        MAX_FRAMES
                    )));
                }
                // Save state
                let saved_frame_count = self.frames.len();
                let func_slot = self.stack.len();
                // Push a dummy for the function slot
                self.push(Value::Unit);
                for arg in args {
                    self.push(arg.clone());
                }
                self.frames.push(CallFrame {
                    closure: closure.clone(),
                    ip: 0,
                    base_slot: func_slot + 1,
                });
                // Run the execution loop until we return to the previous frame count
                loop {
                    let op_byte = self.read_byte()?;
                    let op = Op::from_byte(op_byte).ok_or_else(|| {
                        self.frames.truncate(saved_frame_count);
                        self.stack.truncate(func_slot);
                        VmError::new(format!("unknown opcode: {op_byte}"))
                    })?;
                    match self.dispatch_one(op) {
                        Ok(DispatchResult::Continue) => {}
                        Ok(DispatchResult::Return(result)) => {
                            let finished_base = self.current_frame()?.base_slot;
                            self.frames.pop();
                            if self.frames.len() < saved_frame_count {
                                return Err(VmError::new(
                                    "frame underflow in invoke_callable".into(),
                                ));
                            }
                            if self.frames.len() == saved_frame_count {
                                // We've returned from our closure
                                self.stack.truncate(func_slot);
                                return Ok(result);
                            }
                            // Inner return from nested call
                            let inner_func_slot = finished_base.saturating_sub(1);
                            self.stack.truncate(inner_func_slot);
                            self.push(result);
                        }
                        Ok(DispatchResult::EarlyReturn {
                            value,
                            finished_base,
                        }) => {
                            // QuestionMark popped a frame. Check if we've returned to our level.
                            if self.frames.len() <= saved_frame_count {
                                self.stack.truncate(func_slot);
                                return Ok(value);
                            }
                            // Inner early return
                            let inner_func_slot = finished_base.saturating_sub(1);
                            self.stack.truncate(inner_func_slot);
                            self.push(value);
                        }
                        Err(e) => {
                            self.frames.truncate(saved_frame_count);
                            self.stack.truncate(func_slot);
                            return Err(e);
                        }
                    }
                }
            }
            Value::BuiltinFn(name) => self.dispatch_builtin(name, args),
            Value::VariantConstructor(name, arity) => {
                if args.len() != *arity {
                    return Err(VmError::new(format!(
                        "variant constructor '{name}' expects {arity} arguments, got {}",
                        args.len()
                    )));
                }
                Ok(Value::Variant(name.clone(), args.to_vec()))
            }
            _ => Err(VmError::new(
                "cannot call value in invoke_callable".to_string(),
            )),
        }
    }

    /// Dispatch a single opcode. All three execution loops call this.
    pub(super) fn dispatch_one(&mut self, op: Op) -> Result<DispatchResult, VmError> {
        match op {
            Op::Constant => {
                let index = self.read_u16()? as usize;
                let value = self.read_constant(index)?;
                self.push(value);
            }
            Op::Unit => self.push(Value::Unit),
            Op::True => self.push(Value::Bool(true)),
            Op::False => self.push(Value::Bool(false)),
            Op::Add => self.binary_arithmetic(Op::Add)?,
            Op::Sub => self.binary_arithmetic(Op::Sub)?,
            Op::Mul => self.binary_arithmetic(Op::Mul)?,
            Op::Div => self.binary_arithmetic(Op::Div)?,
            Op::Mod => self.binary_arithmetic(Op::Mod)?,
            Op::Eq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.check_same_type(&a, &b)?;
                self.push(Value::Bool(a == b));
            }
            Op::Neq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.check_same_type(&a, &b)?;
                self.push(Value::Bool(a != b));
            }
            Op::Lt => self.compare(|ord| ord.is_lt())?,
            Op::Gt => self.compare(|ord| ord.is_gt())?,
            Op::Leq => self.compare(|ord| ord.is_le())?,
            Op::Geq => self.compare(|ord| ord.is_ge())?,
            Op::Negate => {
                let val = self.pop()?;
                match val {
                    Value::Int(n) => match n.checked_neg() {
                        Some(v) => self.push(Value::Int(v)),
                        None => {
                            return Err(VmError::new(format!("integer overflow: -{n}")));
                        }
                    },
                    Value::Float(n) => {
                        let result = if -n == 0.0 { 0.0 } else { -n };
                        self.push(Value::Float(result));
                    }
                    Value::ExtFloat(n) => {
                        self.push(Value::ExtFloat(-n));
                    }
                    other => {
                        return Err(VmError::new(format!(
                            "cannot negate {}",
                            self.type_name(&other)
                        )));
                    }
                }
            }
            Op::Not => {
                let val = self.pop()?;
                match val {
                    Value::Bool(b) => self.push(Value::Bool(!b)),
                    other => {
                        return Err(VmError::new(format!(
                            "cannot apply 'not' to {}",
                            self.type_name(&other)
                        )));
                    }
                }
            }
            Op::And => {
                let b = self.pop()?;
                let a = self.pop()?;
                match (&a, &b) {
                    (Value::Bool(a_val), Value::Bool(b_val)) => {
                        self.push(Value::Bool(*a_val && *b_val))
                    }
                    _ => return Err(VmError::new("logical 'and' requires two booleans".into())),
                }
            }
            Op::Or => {
                let b = self.pop()?;
                let a = self.pop()?;
                match (&a, &b) {
                    (Value::Bool(a_val), Value::Bool(b_val)) => {
                        self.push(Value::Bool(*a_val || *b_val))
                    }
                    _ => return Err(VmError::new("logical 'or' requires two booleans".into())),
                }
            }
            Op::DisplayValue => {
                let val = self.pop()?;
                if matches!(&val, Value::String(_)) {
                    self.push(val);
                } else {
                    let s = self.display_value(&val);
                    self.push(Value::String(s));
                }
            }
            Op::StringConcat => {
                let count = self.read_u8()? as usize;
                if count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "StringConcat: need {} values but stack has {}",
                        count,
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - count;
                // Pre-calculate total capacity to avoid reallocations
                let mut total_len = 0;
                for i in start..self.stack.len() {
                    if let Value::String(ref s) = self.stack[i] {
                        total_len += s.len();
                    } else {
                        return Err(VmError::new(
                            "StringConcat: non-string value on stack".into(),
                        ));
                    }
                }
                let mut result = String::with_capacity(total_len);
                for i in start..self.stack.len() {
                    if let Value::String(ref s) = self.stack[i] {
                        result.push_str(s);
                    }
                }
                self.stack.truncate(start);
                self.push(Value::String(result));
            }
            Op::GetLocal => {
                let slot = self.read_u16()? as usize;
                let base = self.current_frame()?.base_slot;
                let value = self
                    .stack
                    .get(base + slot)
                    .ok_or_else(|| {
                        VmError::new(format!(
                            "stack index out of bounds (slot {slot}, base {base}, stack len {})",
                            self.stack.len()
                        ))
                    })?
                    .clone();
                self.push(value);
            }
            Op::SetLocal => {
                let slot = self.read_u16()? as usize;
                let base = self.current_frame()?.base_slot;
                let value = self.peek()?.clone();
                let target = base + slot;
                while self.stack.len() <= target {
                    self.stack.push(Value::Unit);
                }
                self.stack[target] = value;
            }
            Op::GetGlobal => {
                let name_index = self.read_u16()? as usize;
                let name = self.read_constant_string(name_index)?;
                let value = self
                    .globals
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| VmError::new(format!("undefined global: {name}")))?;
                self.push(value);
            }
            Op::SetGlobal => {
                let name_index = self.read_u16()? as usize;
                let name = self.read_constant_string(name_index)?;
                let value = self.peek()?.clone();
                self.globals.insert(name, value);
            }
            Op::GetUpvalue => {
                let index = self.read_u8()? as usize;
                let upvalues = &self.current_frame()?.closure.upvalues;
                let value = upvalues
                    .get(index)
                    .ok_or_else(|| {
                        VmError::new(format!(
                            "upvalue index {index} out of bounds (count {})",
                            upvalues.len()
                        ))
                    })?
                    .clone();
                self.push(value);
            }
            Op::Call => {
                let argc = self.read_u8()? as usize;
                if argc + 1 > self.stack.len() {
                    return Err(VmError::new(format!(
                        "call: argc {argc} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let func_slot = self.stack.len() - 1 - argc;
                let func_val = self.stack[func_slot].clone();
                self.call_value(func_val, argc, func_slot)?;
            }
            Op::TailCall => {
                let argc = self.read_u8()? as usize;
                if argc + 1 > self.stack.len() {
                    return Err(VmError::new(format!(
                        "tail call: argc {argc} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let func_slot = self.stack.len() - 1 - argc;
                let func_val = self.stack[func_slot].clone();
                if let Value::VmClosure(closure) = func_val {
                    if argc != closure.function.arity as usize {
                        return Err(VmError::new(format!(
                            "function '{}' expects {} arguments, got {}",
                            closure.function.name, closure.function.arity, argc
                        )));
                    }
                    let base = self.current_frame()?.base_slot;
                    for i in 0..argc {
                        self.stack[base + i] = self.stack[func_slot + 1 + i].clone();
                    }
                    self.stack.truncate(base + argc);
                    let frame = self.current_frame_mut()?;
                    frame.closure = closure;
                    frame.ip = 0;
                } else {
                    self.call_value(func_val, argc, func_slot)?;
                }
            }
            Op::Return => {
                let result = self.pop()?;
                return Ok(DispatchResult::Return(result));
            }
            Op::CallBuiltin => {
                let name_index = self.read_u16()? as usize;
                let argc = self.read_u8()? as usize;
                let name = self.read_constant_string(name_index)?;
                if argc > self.stack.len() {
                    return Err(VmError::new(format!(
                        "call builtin '{name}': argc {argc} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - argc;
                let args: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                let result = self.dispatch_builtin(&name, &args)?;
                self.push(result);
            }
            Op::MakeClosure => {
                let func_index = self.read_u16()? as usize;
                let upvalue_count = self.read_u8()? as usize;
                let constant = self.read_constant(func_index)?;
                let mut upvalues = Vec::with_capacity(upvalue_count);
                for _ in 0..upvalue_count {
                    let is_local = self.read_u8()? != 0;
                    let index = self.read_u8()? as usize;
                    let val = if is_local {
                        let base = self.current_frame()?.base_slot;
                        self.stack.get(base + index)
                            .ok_or_else(|| VmError::new(format!(
                                "closure capture: stack index out of bounds (index {index}, base {base}, stack len {})",
                                self.stack.len()
                            )))?
                            .clone()
                    } else {
                        let upvalues = &self.current_frame()?.closure.upvalues;
                        upvalues.get(index)
                            .ok_or_else(|| VmError::new(format!(
                                "closure capture: upvalue index {index} out of bounds (count {})",
                                upvalues.len()
                            )))?
                            .clone()
                    };
                    upvalues.push(val);
                }
                if let Value::VmClosure(existing) = constant {
                    let closure = Arc::new(VmClosure {
                        function: existing.function.clone(),
                        upvalues,
                    });
                    self.push(Value::VmClosure(closure));
                } else {
                    return Err(VmError::new(
                        "internal: MakeClosure constant is not a VmClosure".to_string(),
                    ));
                }
            }
            Op::MakeTuple => {
                let count = self.read_u8()? as usize;
                if count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "MakeTuple: count {count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - count;
                let elements: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::Tuple(elements));
            }
            Op::MakeList => {
                let count = self.read_u16()? as usize;
                if count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "MakeList: count {count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - count;
                let elements: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::List(Arc::new(elements)));
            }
            Op::MakeMap => {
                let pair_count = self.read_u16()? as usize;
                let total = pair_count * 2;
                if total > self.stack.len() {
                    return Err(VmError::new(format!(
                        "MakeMap: need {total} values but stack has {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - total;
                let mut map = BTreeMap::new();
                for i in (start..self.stack.len()).step_by(2) {
                    map.insert(self.stack[i].clone(), self.stack[i + 1].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Map(Arc::new(map)));
            }
            Op::MakeSet => {
                let count = self.read_u16()? as usize;
                if count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "MakeSet: count {count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - count;
                let mut set = BTreeSet::new();
                for i in start..self.stack.len() {
                    set.insert(self.stack[i].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Set(Arc::new(set)));
            }
            Op::MakeRecord => {
                let type_name_index = self.read_u16()? as usize;
                let field_count = self.read_u8()? as usize;
                let mut field_names = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let name_index = self.read_u16()? as usize;
                    field_names.push(self.read_constant_string(name_index)?);
                }
                let type_name = self.read_constant_string(type_name_index)?;
                if field_count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "MakeRecord: field count {field_count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - field_count;
                let mut fields = BTreeMap::new();
                for (i, name) in field_names.into_iter().enumerate() {
                    fields.insert(name, self.stack[start + i].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Record(type_name, Arc::new(fields)));
            }
            Op::MakeVariant => {
                let name_index = self.read_u16()? as usize;
                let field_count = self.read_u8()? as usize;
                let name = self.read_constant_string(name_index)?;
                if field_count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "MakeVariant: field count {field_count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - field_count;
                let fields: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::Variant(name, fields));
            }
            Op::RecordUpdate => {
                let field_count = self.read_u8()? as usize;
                let mut field_names = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let ni = self.read_u16()? as usize;
                    field_names.push(self.read_constant_string(ni)?);
                }
                if field_count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "RecordUpdate: field count {field_count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - field_count;
                let new_values: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                let base = self.pop()?;
                if let Value::Record(type_name, mut existing) = base {
                    let fields = Arc::make_mut(&mut existing);
                    for (name, val) in field_names.into_iter().zip(new_values) {
                        fields.insert(name, val);
                    }
                    self.push(Value::Record(type_name, existing));
                } else {
                    return Err(VmError::new("record update on non-record".into()));
                }
            }
            Op::MakeRange => {
                let end = self.pop()?;
                let start = self.pop()?;
                if let (Value::Int(a), Value::Int(b)) = (&start, &end) {
                    self.push(Value::Range(*a, *b));
                } else {
                    return Err(VmError::new("range requires two integers".into()));
                }
            }
            Op::ListConcat => {
                let b = self.pop()?;
                let a = self.pop()?;
                let mut result = match a {
                    Value::List(xs) => xs.as_ref().clone(),
                    Value::Range(lo, hi) => {
                        checked_range_len(lo, hi).map_err(VmError::new)?;
                        (lo..=hi).map(Value::Int).collect()
                    }
                    _ => {
                        return Err(VmError::new(
                            "ListConcat: left operand is not a list or range".into(),
                        ));
                    }
                };
                match b {
                    Value::List(xs) => result.extend(xs.iter().cloned()),
                    Value::Range(lo, hi) => {
                        checked_range_len(lo, hi).map_err(VmError::new)?;
                        result.extend((lo..=hi).map(Value::Int));
                    }
                    _ => {
                        return Err(VmError::new(
                            "ListConcat: right operand is not a list or range".into(),
                        ));
                    }
                }
                self.push(Value::List(Arc::new(result)));
            }
            Op::GetField => {
                let name_index = self.read_u16()? as usize;
                let name = self.read_constant_string(name_index)?;
                let target = self.pop()?;
                match target {
                    Value::Record(_, ref fields) => {
                        let val = fields
                            .get(&name)
                            .cloned()
                            .ok_or_else(|| VmError::new(format!("record has no field '{name}'")))?;
                        self.push(val);
                    }
                    Value::Map(ref map) => {
                        let val = map
                            .get(&Value::String(name.clone()))
                            .cloned()
                            .ok_or_else(|| VmError::new(format!("map has no key '{name}'")))?;
                        self.push(val);
                    }
                    other => {
                        return Err(VmError::new(format!(
                            "cannot access field '{}' on {}",
                            name,
                            self.type_name(&other)
                        )));
                    }
                }
            }
            Op::GetIndex => {
                let index = self.read_u8()? as usize;
                let target = self.pop()?;
                if let Value::Tuple(ref elems) = target {
                    let val = elems
                        .get(index)
                        .cloned()
                        .ok_or_else(|| VmError::new("tuple index out of bounds".to_string()))?;
                    self.push(val);
                } else {
                    return Err(VmError::new(format!(
                        "cannot index into {}",
                        self.type_name(&target)
                    )));
                }
            }
            Op::Jump => {
                let offset = self.read_u16()? as usize;
                self.current_frame_mut()?.ip += offset;
            }
            Op::JumpBack => {
                let offset = self.read_u16()? as usize;
                self.current_frame_mut()?.ip -= offset;
            }
            Op::JumpIfFalse => {
                let offset = self.read_u16()? as usize;
                let val = self.pop()?;
                if self.is_falsy(&val) {
                    self.current_frame_mut()?.ip += offset;
                }
            }
            Op::JumpIfTrue => {
                let offset = self.read_u16()? as usize;
                let val = self.pop()?;
                if self.is_truthy(&val) {
                    self.current_frame_mut()?.ip += offset;
                }
            }
            Op::Pop => {
                self.pop()?;
            }
            Op::PopN => {
                let count = self.read_u8()? as usize;
                let new_len = self.stack.len().saturating_sub(count);
                self.stack.truncate(new_len);
            }
            Op::Dup => {
                let val = self.peek()?.clone();
                self.push(val);
            }
            Op::TestTag => {
                let ni = self.read_u16()? as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek()?;
                let result = matches!(val, Value::Variant(tag, _) if *tag == name);
                self.push(Value::Bool(result));
            }
            Op::TestEqual => {
                let ci = self.read_u16()? as usize;
                let constant = self.read_constant(ci)?;
                let val = self.peek()?;
                let result = *val == constant;
                self.push(Value::Bool(result));
            }
            Op::TestTupleLen => {
                let len = self.read_u8()? as usize;
                let val = self.peek()?;
                let result = matches!(val, Value::Tuple(elems) if elems.len() == len);
                self.push(Value::Bool(result));
            }
            Op::TestListMin => {
                let min_len = self.read_u8()? as usize;
                let val = self.peek()?;
                let result = val.collection_len().is_some_and(|len| len >= min_len);
                self.push(Value::Bool(result));
            }
            Op::TestListExact => {
                let len = self.read_u8()? as usize;
                let val = self.peek()?;
                let result = val.collection_len() == Some(len);
                self.push(Value::Bool(result));
            }
            Op::TestIntRange => {
                let lo_index = self.read_u16()? as usize;
                let hi_index = self.read_u16()? as usize;
                let lo = self.read_constant(lo_index)?;
                let hi = self.read_constant(hi_index)?;
                let val = self.peek()?;
                let result = match (val, &lo, &hi) {
                    (Value::Int(n), Value::Int(lo), Value::Int(hi)) => *n >= *lo && *n <= *hi,
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::TestFloatRange => {
                let lo_index = self.read_u16()? as usize;
                let hi_index = self.read_u16()? as usize;
                let lo = self.read_constant(lo_index)?;
                let hi = self.read_constant(hi_index)?;
                let val = self.peek()?;
                let result = match (val, &lo, &hi) {
                    (Value::Float(n), Value::Float(lo), Value::Float(hi)) => *n >= *lo && *n <= *hi,
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::TestBool => {
                let expected = self.read_u8()? != 0;
                let val = self.peek()?;
                let result = matches!(val, Value::Bool(b) if *b == expected);
                self.push(Value::Bool(result));
            }
            Op::DestructTuple => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                if let Value::Tuple(elems) = val {
                    let elem = elems.get(index).ok_or_else(|| {
                        VmError::new(format!(
                            "DestructTuple index {} out of bounds (len {})",
                            index,
                            elems.len()
                        ))
                    })?;
                    self.push(elem.clone());
                } else {
                    return Err(VmError::new("DestructTuple on non-tuple".into()));
                }
            }
            Op::DestructVariant => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                if let Value::Variant(_, fields) = val {
                    let field = fields.get(index).ok_or_else(|| {
                        VmError::new(format!(
                            "DestructVariant index {} out of bounds (len {})",
                            index,
                            fields.len()
                        ))
                    })?;
                    self.push(field.clone());
                } else {
                    return Err(VmError::new("DestructVariant on non-variant".into()));
                }
            }
            Op::DestructList => {
                let index = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                match val {
                    Value::List(xs) => {
                        let elem = xs.get(index).ok_or_else(|| {
                            VmError::new(format!(
                                "DestructList index {} out of bounds (len {})",
                                index,
                                xs.len()
                            ))
                        })?;
                        self.push(elem.clone());
                    }
                    Value::Range(lo, hi) => {
                        let i = lo
                            .checked_add(index as i64)
                            .ok_or_else(|| VmError::new("range index overflow".to_string()))?;
                        if i > hi {
                            return Err(VmError::new("range index out of bounds".into()));
                        }
                        self.push(Value::Int(i));
                    }
                    _ => return Err(VmError::new("DestructList on non-list".into())),
                }
            }
            Op::DestructListRest => {
                let start = self.read_u8()? as usize;
                let val = self.peek()?.clone();
                match val {
                    Value::List(xs) => {
                        if start > xs.len() {
                            return Err(VmError::new(format!(
                                "DestructListRest start {} out of bounds (len {})",
                                start,
                                xs.len()
                            )));
                        }
                        self.push(Value::List(Arc::new(xs[start..].to_vec())));
                    }
                    Value::Range(lo, hi) => {
                        let new_lo = lo
                            .checked_add(start as i64)
                            .ok_or_else(|| VmError::new("range index overflow".to_string()))?;
                        if new_lo > hi + 1 {
                            self.push(Value::List(Arc::new(Vec::new())));
                        } else {
                            self.push(Value::Range(new_lo, hi));
                        }
                    }
                    _ => return Err(VmError::new("DestructListRest on non-list".into())),
                }
            }
            Op::DestructRecordField => {
                let ni = self.read_u16()? as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek()?.clone();
                if let Value::Record(_, fields) = val {
                    let field = fields
                        .get(&name)
                        .cloned()
                        .ok_or_else(|| VmError::new(format!("record has no field '{name}'")))?;
                    self.push(field);
                } else {
                    return Err(VmError::new("DestructRecordField on non-record".into()));
                }
            }
            Op::TestRecordTag => {
                let ni = self.read_u16()? as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek()?;
                let result = matches!(val, Value::Record(tag, _) if *tag == name);
                self.push(Value::Bool(result));
            }
            Op::TestMapHasKey => {
                let ci = self.read_u16()? as usize;
                let key_name = self.read_constant_string(ci)?;
                let val = self.peek()?;
                let result = match val {
                    Value::Map(map) => map.contains_key(&Value::String(key_name)),
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::DestructMapValue => {
                let ci = self.read_u16()? as usize;
                let key_name = self.read_constant_string(ci)?;
                let val = self.peek()?.clone();
                if let Value::Map(map) = val {
                    let value = map
                        .get(&Value::String(key_name.clone()))
                        .cloned()
                        .ok_or_else(|| VmError::new(format!("map has no key '{key_name}'")))?;
                    self.push(value);
                } else {
                    return Err(VmError::new("DestructMapValue on non-map".into()));
                }
            }
            Op::LoopSetup => {
                let _ = self.read_u8()?;
            }
            Op::Recur => {
                let arg_count = self.read_u8()? as usize;
                let first_slot = self.read_u16()? as usize;
                let base = self.current_frame()?.base_slot;
                if arg_count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "recur: arg count {arg_count} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let start = self.stack.len() - arg_count;
                let dest_end = base + first_slot + arg_count;
                if dest_end > self.stack.len() || start + arg_count > self.stack.len() {
                    return Err(VmError::new(format!(
                        "recur: destination slot out of bounds (base {base}, first_slot {first_slot}, arg_count {arg_count}, stack len {})",
                        self.stack.len()
                    )));
                }
                for i in 0..arg_count {
                    self.stack[base + first_slot + i] = self.stack[start + i].clone();
                }
                // Truncate all the way back to just after loop bindings.
                self.stack.truncate(base + first_slot + arg_count);
            }
            Op::QuestionMark => {
                let val = self.peek()?.clone();
                match val {
                    Value::Variant(ref tag, ref fields) => match tag.as_str() {
                        "Ok" | "Some" => {
                            self.pop()?;
                            self.push(if fields.len() == 1 {
                                fields[0].clone()
                            } else {
                                Value::Unit
                            });
                        }
                        "Err" | "None" => {
                            let value = self.pop()?;
                            let finished_base = self.current_frame()?.base_slot;
                            self.frames.pop();
                            return Ok(DispatchResult::EarlyReturn {
                                value,
                                finished_base,
                            });
                        }
                        _ => return Err(VmError::new(format!("? on non-Result/Option: {tag}"))),
                    },
                    _ => {
                        return Err(VmError::new(format!(
                            "? on non-variant: {}",
                            self.type_name(&val)
                        )));
                    }
                }
            }
            Op::Panic => {
                let msg = self.pop()?;
                return Err(VmError::new(format!("panic: {}", self.display_value(&msg))));
            }
            Op::CallMethod => {
                let method_name_index = self.read_u16()? as usize;
                let argc = self.read_u8()? as usize;
                let method_name = self.read_constant_string(method_name_index)?;
                if argc > self.stack.len() {
                    return Err(VmError::new(format!(
                        "call method '{method_name}': argc {argc} exceeds stack size {}",
                        self.stack.len()
                    )));
                }
                let receiver_slot = self.stack.len() - argc;
                let receiver = self.stack[receiver_slot].clone();
                let type_name = self.value_type_name_for_dispatch(&receiver);
                let qualified = format!("{type_name}.{method_name}");
                if let Some(func) = self.globals.get(&qualified).cloned() {
                    let args: Vec<Value> = self.stack[receiver_slot..].to_vec();
                    self.stack.truncate(receiver_slot);
                    let result = self.invoke_callable(&func, &args)?;
                    self.push(result);
                } else {
                    let extra_args: Vec<Value> = self.stack[receiver_slot + 1..].to_vec();
                    // Try built-in trait methods (display, equal, compare)
                    if let Some(result) =
                        self.dispatch_trait_method(&receiver, &method_name, &extra_args)
                    {
                        self.stack.truncate(receiver_slot);
                        self.push(result?);
                    } else if let Value::Record(_, ref fields) = receiver {
                        if let Some(field_val) = fields.get(&method_name) {
                            let callable = field_val.clone();
                            self.stack.truncate(receiver_slot);
                            let result = self.invoke_callable(&callable, &extra_args)?;
                            self.push(result);
                        } else {
                            return Err(VmError::new(format!(
                                "no method '{method_name}' for type '{type_name}'"
                            )));
                        }
                    } else {
                        return Err(VmError::new(format!(
                            "no method '{method_name}' for type '{type_name}'"
                        )));
                    }
                }
            }
            Op::NarrowFloat => {
                let offset = self.read_u16()? as usize;
                let val = self.peek()?.clone();
                match val {
                    Value::ExtFloat(f) if f.is_finite() => {
                        self.pop()?;
                        self.push(Value::Float(if f == 0.0 { 0.0 } else { f }));
                        self.current_frame_mut()?.ip += offset;
                    }
                    Value::ExtFloat(_) => {
                        self.pop()?;
                    }
                    Value::Float(_) => {
                        self.current_frame_mut()?.ip += offset;
                    }
                    _ => {
                        return Err(VmError::new(
                            "NarrowFloat: expected float value".to_string(),
                        ));
                    }
                }
            }
        }
        Ok(DispatchResult::Continue)
    }
}
