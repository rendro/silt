//! Stack-based bytecode VM for Silt.
//!
//! Executes compiled `Function` objects produced by the compiler.

use std::collections::HashMap;
use std::rc::Rc;

use crate::bytecode::{Chunk, Function, Op, VmClosure};
use crate::value::Value;

// ── Error type ────────────────────────────────────────────────────

#[derive(Debug)]
pub struct VmError {
    pub message: String,
}

impl VmError {
    pub fn new(message: String) -> Self {
        VmError { message }
    }
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VM error: {}", self.message)
    }
}

impl std::error::Error for VmError {}

// ── Call frame ────────────────────────────────────────────────────

struct CallFrame {
    closure: Rc<VmClosure>,
    ip: usize,
    base_slot: usize,
}

// ── VM ────────────────────────────────────────────────────────────

pub struct Vm {
    frames: Vec<CallFrame>,
    stack: Vec<Value>,
    globals: HashMap<String, Value>,
}

impl Vm {
    pub fn new() -> Self {
        Vm {
            frames: Vec::new(),
            stack: Vec::new(),
            globals: HashMap::new(),
        }
    }

    /// Load a compiled top-level function and execute it.
    pub fn run(&mut self, script: Rc<Function>) -> Result<Value, VmError> {
        let closure = Rc::new(VmClosure {
            function: script,
            upvalues: vec![],
        });
        self.frames.push(CallFrame {
            closure,
            ip: 0,
            base_slot: 0,
        });
        self.execute()
    }

    // ── Main execution loop ───────────────────────────────────────

    fn execute(&mut self) -> Result<Value, VmError> {
        loop {
            let op_byte = self.read_byte();
            match Op::from_byte(op_byte) {
                // ── Constants & literals ───────────────────────
                Some(Op::Constant) => {
                    let index = self.read_u16() as usize;
                    let value = self.read_constant(index);
                    self.push(value);
                }
                Some(Op::Unit) => self.push(Value::Unit),
                Some(Op::True) => self.push(Value::Bool(true)),
                Some(Op::False) => self.push(Value::Bool(false)),

                // ── Arithmetic ────────────────────────────────
                Some(Op::Add) => self.binary_arithmetic(Op::Add)?,
                Some(Op::Sub) => self.binary_arithmetic(Op::Sub)?,
                Some(Op::Mul) => self.binary_arithmetic(Op::Mul)?,
                Some(Op::Div) => self.binary_arithmetic(Op::Div)?,
                Some(Op::Mod) => self.binary_arithmetic(Op::Mod)?,

                // ── Comparison ────────────────────────────────
                Some(Op::Eq) => {
                    let b = self.pop();
                    let a = self.pop();
                    self.push(Value::Bool(a == b));
                }
                Some(Op::Neq) => {
                    let b = self.pop();
                    let a = self.pop();
                    self.push(Value::Bool(a != b));
                }
                Some(Op::Lt) => self.compare(|ord| ord.is_lt())?,
                Some(Op::Gt) => self.compare(|ord| ord.is_gt())?,
                Some(Op::Leq) => self.compare(|ord| ord.is_le())?,
                Some(Op::Geq) => self.compare(|ord| ord.is_ge())?,

                // ── Unary ─────────────────────────────────────
                Some(Op::Negate) => {
                    let val = self.pop();
                    match val {
                        Value::Int(n) => self.push(Value::Int(-n)),
                        Value::Float(n) => self.push(Value::Float(-n)),
                        other => {
                            return Err(VmError::new(format!(
                                "cannot negate {}",
                                self.type_name(&other)
                            )))
                        }
                    }
                }
                Some(Op::Not) => {
                    let val = self.pop();
                    match val {
                        Value::Bool(b) => self.push(Value::Bool(!b)),
                        other => {
                            return Err(VmError::new(format!(
                                "cannot apply 'not' to {}",
                                self.type_name(&other)
                            )))
                        }
                    }
                }

                // ── Logical ───────────────────────────────────
                Some(Op::And) => {
                    let b = self.pop();
                    let a = self.pop();
                    match (&a, &b) {
                        (Value::Bool(a_val), Value::Bool(b_val)) => {
                            self.push(Value::Bool(*a_val && *b_val));
                        }
                        _ => {
                            return Err(VmError::new(
                                "logical 'and' requires two booleans".to_string(),
                            ))
                        }
                    }
                }
                Some(Op::Or) => {
                    let b = self.pop();
                    let a = self.pop();
                    match (&a, &b) {
                        (Value::Bool(a_val), Value::Bool(b_val)) => {
                            self.push(Value::Bool(*a_val || *b_val));
                        }
                        _ => {
                            return Err(VmError::new(
                                "logical 'or' requires two booleans".to_string(),
                            ))
                        }
                    }
                }

                // ── String interpolation ──────────────────────
                Some(Op::DisplayValue) => {
                    let val = self.pop();
                    let s = self.display_value(&val);
                    self.push(Value::String(s));
                }
                Some(Op::StringConcat) => {
                    let count = self.read_u8() as usize;
                    let start = self.stack.len() - count;
                    let mut result = String::new();
                    for i in start..self.stack.len() {
                        if let Value::String(ref s) = self.stack[i] {
                            result.push_str(s);
                        } else {
                            return Err(VmError::new(
                                "StringConcat: non-string value on stack".to_string(),
                            ));
                        }
                    }
                    self.stack.truncate(start);
                    self.push(Value::String(result));
                }

                // ── Variables ─────────────────────────────────
                Some(Op::GetLocal) => {
                    let slot = self.read_u16() as usize;
                    let base = self.current_frame().base_slot;
                    let value = self.stack[base + slot].clone();
                    self.push(value);
                }
                Some(Op::SetLocal) => {
                    let slot = self.read_u16() as usize;
                    let base = self.current_frame().base_slot;
                    let value = self.peek().clone();
                    self.stack[base + slot] = value;
                }
                Some(Op::GetGlobal) => {
                    let name_index = self.read_u16() as usize;
                    let name = self.read_constant_string(name_index)?;
                    let value = self.globals.get(&name).cloned().ok_or_else(|| {
                        VmError::new(format!("undefined global: {name}"))
                    })?;
                    self.push(value);
                }
                Some(Op::SetGlobal) => {
                    let name_index = self.read_u16() as usize;
                    let name = self.read_constant_string(name_index)?;
                    let value = self.pop();
                    self.globals.insert(name, value);
                }

                // ── Upvalues ──────────────────────────────────
                Some(Op::GetUpvalue) => {
                    let index = self.read_u8() as usize;
                    let value = self.current_frame().closure.upvalues[index].clone();
                    self.push(value);
                }

                // ── Function calls ────────────────────────────
                Some(Op::Call) => {
                    let argc = self.read_u8() as usize;
                    // The function value sits below the arguments on the stack.
                    let func_slot = self.stack.len() - 1 - argc;
                    let func_val = self.stack[func_slot].clone();
                    match func_val {
                        Value::Closure(_) => {
                            // The tree-walking interpreter uses Closure; not callable in VM.
                            return Err(VmError::new(
                                "cannot call tree-walking Closure in VM".to_string(),
                            ));
                        }
                        _ => {
                            // Try to interpret the value as containing a VmClosure.
                            // In Phase 1, closures on the stack are stored as constants
                            // holding a Function Rc. The compiler will emit MakeClosure
                            // or store VmClosures as constants.
                            //
                            // For now, we handle the case where the compiler pushes
                            // a Function wrapped in a closure constant.
                            return Err(VmError::new(format!(
                                "cannot call value of type {}",
                                self.type_name(&func_val)
                            )));
                        }
                    }
                }
                Some(Op::TailCall) => {
                    let argc = self.read_u8() as usize;
                    let func_slot = self.stack.len() - 1 - argc;
                    let func_val = self.stack[func_slot].clone();
                    match func_val {
                        _ => {
                            return Err(VmError::new(format!(
                                "cannot tail-call value of type {}",
                                self.type_name(&func_val)
                            )));
                        }
                    }
                }
                Some(Op::Return) => {
                    let result = self.pop();
                    let finished_base = self.current_frame().base_slot;
                    self.frames.pop();
                    if self.frames.is_empty() {
                        return Ok(result);
                    }
                    // Discard the callee's stack window (including the function slot).
                    self.stack.truncate(finished_base);
                    self.push(result);
                }
                Some(Op::CallBuiltin) => {
                    let name_index = self.read_u16() as usize;
                    let argc = self.read_u8() as usize;
                    let name = self.read_constant_string(name_index)?;
                    let start = self.stack.len() - argc;
                    let args: Vec<Value> = self.stack[start..].to_vec();
                    self.stack.truncate(start);
                    let result = self.dispatch_builtin(&name, &args)?;
                    self.push(result);
                }

                // ── Closures ──────────────────────────────────
                Some(Op::MakeClosure) => {
                    let func_index = self.read_u16() as usize;
                    let upvalue_count = self.read_u8() as usize;
                    // The constant pool may not directly hold Functions in Value form
                    // yet. For Phase 1, we skip full closure support. Just consume the
                    // upvalue descriptors and push Unit as a placeholder.
                    let _constant = self.read_constant(func_index);
                    for _ in 0..upvalue_count {
                        let _is_local = self.read_u8();
                        let _index = self.read_u8();
                    }
                    self.push(Value::Unit);
                }

                // ── Data constructors ─────────────────────────
                Some(Op::MakeTuple) => {
                    let count = self.read_u8() as usize;
                    let start = self.stack.len() - count;
                    let elements: Vec<Value> = self.stack[start..].to_vec();
                    self.stack.truncate(start);
                    self.push(Value::Tuple(elements));
                }
                Some(Op::MakeList) => {
                    let count = self.read_u16() as usize;
                    let start = self.stack.len() - count;
                    let elements: Vec<Value> = self.stack[start..].to_vec();
                    self.stack.truncate(start);
                    self.push(Value::List(Rc::new(elements)));
                }
                Some(Op::MakeMap) => {
                    let pair_count = self.read_u16() as usize;
                    let total = pair_count * 2;
                    let start = self.stack.len() - total;
                    let mut map = std::collections::BTreeMap::new();
                    for i in (start..self.stack.len()).step_by(2) {
                        let key = self.stack[i].clone();
                        let val = self.stack[i + 1].clone();
                        map.insert(key, val);
                    }
                    self.stack.truncate(start);
                    self.push(Value::Map(Rc::new(map)));
                }
                Some(Op::MakeSet) => {
                    let count = self.read_u16() as usize;
                    let start = self.stack.len() - count;
                    let mut set = std::collections::BTreeSet::new();
                    for i in start..self.stack.len() {
                        set.insert(self.stack[i].clone());
                    }
                    self.stack.truncate(start);
                    self.push(Value::Set(Rc::new(set)));
                }
                Some(Op::MakeRecord) => {
                    let type_name_index = self.read_u16() as usize;
                    let field_count = self.read_u8() as usize;
                    let mut field_names = Vec::with_capacity(field_count);
                    for _ in 0..field_count {
                        let name_index = self.read_u16() as usize;
                        field_names.push(self.read_constant_string(name_index)?);
                    }
                    let type_name = self.read_constant_string(type_name_index)?;
                    let start = self.stack.len() - field_count;
                    let mut fields = std::collections::BTreeMap::new();
                    for (i, name) in field_names.into_iter().enumerate() {
                        fields.insert(name, self.stack[start + i].clone());
                    }
                    self.stack.truncate(start);
                    self.push(Value::Record(type_name, Rc::new(fields)));
                }
                Some(Op::MakeVariant) => {
                    let name_index = self.read_u16() as usize;
                    let field_count = self.read_u8() as usize;
                    let name = self.read_constant_string(name_index)?;
                    let start = self.stack.len() - field_count;
                    let fields: Vec<Value> = self.stack[start..].to_vec();
                    self.stack.truncate(start);
                    self.push(Value::Variant(name, fields));
                }
                Some(Op::RecordUpdate) => {
                    let field_count = self.read_u8() as usize;
                    let mut field_names = Vec::with_capacity(field_count);
                    for _ in 0..field_count {
                        let name_index = self.read_u16() as usize;
                        field_names.push(self.read_constant_string(name_index)?);
                    }
                    // Stack: [base_record, new_val_1, ..., new_val_N]
                    let start = self.stack.len() - field_count;
                    let new_values: Vec<Value> = self.stack[start..].to_vec();
                    self.stack.truncate(start);
                    let base = self.pop();
                    match base {
                        Value::Record(type_name, existing) => {
                            let mut fields = (*existing).clone();
                            for (name, val) in
                                field_names.into_iter().zip(new_values.into_iter())
                            {
                                fields.insert(name, val);
                            }
                            self.push(Value::Record(type_name, Rc::new(fields)));
                        }
                        other => {
                            return Err(VmError::new(format!(
                                "record update on non-record: {}",
                                self.type_name(&other)
                            )));
                        }
                    }
                }
                Some(Op::MakeRange) => {
                    let end = self.pop();
                    let start = self.pop();
                    match (&start, &end) {
                        (Value::Int(a), Value::Int(b)) => {
                            let items: Vec<Value> =
                                (*a..=*b).map(Value::Int).collect();
                            self.push(Value::List(Rc::new(items)));
                        }
                        _ => {
                            return Err(VmError::new(
                                "range requires two integers".to_string(),
                            ));
                        }
                    }
                }

                // ── Field access ──────────────────────────────
                Some(Op::GetField) => {
                    let name_index = self.read_u16() as usize;
                    let name = self.read_constant_string(name_index)?;
                    let target = self.pop();
                    match target {
                        Value::Record(_, ref fields) => {
                            let val = fields.get(&name).cloned().ok_or_else(|| {
                                VmError::new(format!("record has no field '{name}'"))
                            })?;
                            self.push(val);
                        }
                        Value::Map(ref map) => {
                            let key = Value::String(name.clone());
                            let val = map.get(&key).cloned().ok_or_else(|| {
                                VmError::new(format!("map has no key '{name}'"))
                            })?;
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
                Some(Op::GetIndex) => {
                    let index = self.read_u8() as usize;
                    let target = self.pop();
                    match target {
                        Value::Tuple(ref elems) => {
                            let val = elems.get(index).cloned().ok_or_else(|| {
                                VmError::new(format!(
                                    "tuple index {index} out of bounds (len {})",
                                    elems.len()
                                ))
                            })?;
                            self.push(val);
                        }
                        other => {
                            return Err(VmError::new(format!(
                                "cannot index into {}",
                                self.type_name(&other)
                            )));
                        }
                    }
                }

                // ── Control flow ──────────────────────────────
                Some(Op::Jump) => {
                    let offset = self.read_u16() as usize;
                    let frame = self.current_frame_mut();
                    frame.ip += offset;
                }
                Some(Op::JumpBack) => {
                    let offset = self.read_u16() as usize;
                    let frame = self.current_frame_mut();
                    frame.ip -= offset;
                }
                Some(Op::JumpIfFalse) => {
                    let offset = self.read_u16() as usize;
                    let val = self.pop();
                    if self.is_falsy(&val) {
                        let frame = self.current_frame_mut();
                        frame.ip += offset;
                    }
                }
                Some(Op::JumpIfTrue) => {
                    let offset = self.read_u16() as usize;
                    let val = self.pop();
                    if self.is_truthy(&val) {
                        let frame = self.current_frame_mut();
                        frame.ip += offset;
                    }
                }
                Some(Op::Pop) => {
                    self.pop();
                }
                Some(Op::PopN) => {
                    let count = self.read_u8() as usize;
                    let new_len = self.stack.len().saturating_sub(count);
                    self.stack.truncate(new_len);
                }
                Some(Op::Dup) => {
                    let val = self.peek().clone();
                    self.push(val);
                }

                // ── Pattern matching ──────────────────────────
                Some(Op::TestTag) => {
                    let name_index = self.read_u16() as usize;
                    let name = self.read_constant_string(name_index)?;
                    let val = self.peek();
                    let result = matches!(val, Value::Variant(tag, _) if *tag == name);
                    self.push(Value::Bool(result));
                }
                Some(Op::TestEqual) => {
                    let const_index = self.read_u16() as usize;
                    let constant = self.read_constant(const_index);
                    let val = self.peek();
                    let result = *val == constant;
                    self.push(Value::Bool(result));
                }
                Some(Op::TestTupleLen) => {
                    let len = self.read_u8() as usize;
                    let val = self.peek();
                    let result = matches!(val, Value::Tuple(elems) if elems.len() == len);
                    self.push(Value::Bool(result));
                }
                Some(Op::TestListMin) => {
                    let min_len = self.read_u8() as usize;
                    let val = self.peek();
                    let result = matches!(val, Value::List(xs) if xs.len() >= min_len);
                    self.push(Value::Bool(result));
                }
                Some(Op::TestListExact) => {
                    let len = self.read_u8() as usize;
                    let val = self.peek();
                    let result = matches!(val, Value::List(xs) if xs.len() == len);
                    self.push(Value::Bool(result));
                }
                Some(Op::TestIntRange) => {
                    // Read lo and hi from the next two constant indices.
                    let lo_index = self.read_u16() as usize;
                    let hi_index = self.read_u16() as usize;
                    let lo = self.read_constant(lo_index);
                    let hi = self.read_constant(hi_index);
                    let val = self.peek();
                    let result = match (val, &lo, &hi) {
                        (Value::Int(n), Value::Int(lo), Value::Int(hi)) => {
                            *n >= *lo && *n <= *hi
                        }
                        _ => false,
                    };
                    self.push(Value::Bool(result));
                }
                Some(Op::TestFloatRange) => {
                    let lo_index = self.read_u16() as usize;
                    let hi_index = self.read_u16() as usize;
                    let lo = self.read_constant(lo_index);
                    let hi = self.read_constant(hi_index);
                    let val = self.peek();
                    let result = match (val, &lo, &hi) {
                        (Value::Float(n), Value::Float(lo), Value::Float(hi)) => {
                            *n >= *lo && *n <= *hi
                        }
                        _ => false,
                    };
                    self.push(Value::Bool(result));
                }
                Some(Op::TestBool) => {
                    let expected = self.read_u8() != 0;
                    let val = self.peek();
                    let result = matches!(val, Value::Bool(b) if *b == expected);
                    self.push(Value::Bool(result));
                }
                Some(Op::DestructTuple) => {
                    let index = self.read_u8() as usize;
                    let val = self.peek().clone();
                    match val {
                        Value::Tuple(elems) => {
                            self.push(elems[index].clone());
                        }
                        _ => {
                            return Err(VmError::new(
                                "DestructTuple on non-tuple".to_string(),
                            ))
                        }
                    }
                }
                Some(Op::DestructVariant) => {
                    let index = self.read_u8() as usize;
                    let val = self.peek().clone();
                    match val {
                        Value::Variant(_, fields) => {
                            self.push(fields[index].clone());
                        }
                        _ => {
                            return Err(VmError::new(
                                "DestructVariant on non-variant".to_string(),
                            ))
                        }
                    }
                }
                Some(Op::DestructList) => {
                    let index = self.read_u8() as usize;
                    let val = self.peek().clone();
                    match val {
                        Value::List(xs) => {
                            self.push(xs[index].clone());
                        }
                        _ => {
                            return Err(VmError::new(
                                "DestructList on non-list".to_string(),
                            ))
                        }
                    }
                }
                Some(Op::DestructListRest) => {
                    let start = self.read_u8() as usize;
                    let val = self.peek().clone();
                    match val {
                        Value::List(xs) => {
                            let rest: Vec<Value> = xs[start..].to_vec();
                            self.push(Value::List(Rc::new(rest)));
                        }
                        _ => {
                            return Err(VmError::new(
                                "DestructListRest on non-list".to_string(),
                            ))
                        }
                    }
                }
                Some(Op::DestructRecordField) => {
                    let name_index = self.read_u16() as usize;
                    let name = self.read_constant_string(name_index)?;
                    let val = self.peek().clone();
                    match val {
                        Value::Record(_, fields) => {
                            let field = fields.get(&name).cloned().ok_or_else(|| {
                                VmError::new(format!("record has no field '{name}'"))
                            })?;
                            self.push(field);
                        }
                        _ => {
                            return Err(VmError::new(
                                "DestructRecordField on non-record".to_string(),
                            ))
                        }
                    }
                }

                // ── Loop ──────────────────────────────────────
                Some(Op::LoopSetup) => {
                    let _binding_count = self.read_u8();
                    // Loop setup is handled during compilation by placing
                    // bindings in local slots. Nothing to do at runtime.
                }
                Some(Op::Recur) => {
                    let arg_count = self.read_u8() as usize;
                    // Update loop bindings: the new values are on top of stack.
                    // Copy them back into the binding slots at the start of this frame.
                    let base = self.current_frame().base_slot;
                    let start = self.stack.len() - arg_count;
                    for i in 0..arg_count {
                        self.stack[base + i] = self.stack[start + i].clone();
                    }
                    self.stack.truncate(start);
                    // Jump back to the loop body start. The compiler should emit
                    // a JumpBack after Recur, or Recur itself resets ip.
                    // For Phase 1 we assume the compiler emits a JumpBack separately.
                }

                // ── Error handling ────────────────────────────
                Some(Op::QuestionMark) => {
                    let val = self.peek().clone();
                    match val {
                        Value::Variant(ref tag, ref fields) => {
                            match tag.as_str() {
                                "Ok" | "Some" => {
                                    self.pop();
                                    if fields.len() == 1 {
                                        self.push(fields[0].clone());
                                    } else {
                                        self.push(Value::Unit);
                                    }
                                }
                                "Err" | "None" => {
                                    // Early return with the error/none value.
                                    let result = self.pop();
                                    let finished_base = self.current_frame().base_slot;
                                    self.frames.pop();
                                    if self.frames.is_empty() {
                                        return Ok(result);
                                    }
                                    self.stack.truncate(finished_base);
                                    self.push(result);
                                }
                                _ => {
                                    return Err(VmError::new(format!(
                                        "? operator on non-Result/Option variant: {tag}"
                                    )));
                                }
                            }
                        }
                        _ => {
                            return Err(VmError::new(format!(
                                "? operator on non-variant: {}",
                                self.type_name(&val)
                            )));
                        }
                    }
                }
                Some(Op::Panic) => {
                    let msg = self.pop();
                    return Err(VmError::new(format!("panic: {}", self.display_value(&msg))));
                }

                // ── Concurrency (stubs for Phase 1) ───────────
                Some(Op::ChanNew)
                | Some(Op::ChanSend)
                | Some(Op::ChanRecv)
                | Some(Op::ChanClose)
                | Some(Op::ChanTrySend)
                | Some(Op::ChanTryRecv)
                | Some(Op::ChanSelect)
                | Some(Op::TaskSpawn)
                | Some(Op::TaskJoin)
                | Some(Op::TaskCancel)
                | Some(Op::Yield) => {
                    return Err(VmError::new(
                        "concurrency opcodes not yet implemented".to_string(),
                    ));
                }

                None => {
                    return Err(VmError::new(format!("unknown opcode: {op_byte}")));
                }
            }
        }
    }

    // ── Stack operations ──────────────────────────────────────────

    fn push(&mut self, value: Value) {
        self.stack.push(value);
    }

    fn pop(&mut self) -> Value {
        self.stack.pop().expect("stack underflow")
    }

    fn peek(&self) -> &Value {
        self.stack.last().expect("stack underflow")
    }

    // ── Bytecode reading ──────────────────────────────────────────

    fn read_byte(&mut self) -> u8 {
        let frame = self.frames.last().unwrap();
        let ip = frame.ip;
        let byte = frame.closure.function.chunk.code[ip];
        self.frames.last_mut().unwrap().ip = ip + 1;
        byte
    }

    fn read_u8(&mut self) -> u8 {
        self.read_byte()
    }

    fn read_u16(&mut self) -> u16 {
        let lo = self.read_byte() as u16;
        let hi = self.read_byte() as u16;
        lo | (hi << 8)
    }

    fn read_constant(&self, index: usize) -> Value {
        self.current_frame().closure.function.chunk.constants[index].clone()
    }

    fn read_constant_string(&self, index: usize) -> Result<String, VmError> {
        let val = self.read_constant(index);
        match val {
            Value::String(s) => Ok(s),
            other => Err(VmError::new(format!(
                "expected string constant at index {index}, got {}",
                self.type_name(&other)
            ))),
        }
    }

    // ── Frame access ──────────────────────────────────────────────

    fn current_frame(&self) -> &CallFrame {
        self.frames.last().expect("no call frame")
    }

    fn current_frame_mut(&mut self) -> &mut CallFrame {
        self.frames.last_mut().expect("no call frame")
    }

    #[allow(dead_code)]
    fn current_chunk(&self) -> &Chunk {
        &self.current_frame().closure.function.chunk
    }

    // ── Arithmetic helpers ────────────────────────────────────────

    fn binary_arithmetic(&mut self, op: Op) -> Result<(), VmError> {
        let b = self.pop();
        let a = self.pop();
        let result = match (&a, &b) {
            (Value::Int(a), Value::Int(b)) => match op {
                Op::Add => Value::Int(a.wrapping_add(*b)),
                Op::Sub => Value::Int(a.wrapping_sub(*b)),
                Op::Mul => Value::Int(a.wrapping_mul(*b)),
                Op::Div => {
                    if *b == 0 {
                        return Err(VmError::new("division by zero".to_string()));
                    }
                    Value::Int(a / b)
                }
                Op::Mod => {
                    if *b == 0 {
                        return Err(VmError::new("modulo by zero".to_string()));
                    }
                    Value::Int(a % b)
                }
                _ => unreachable!(),
            },
            (Value::Float(a), Value::Float(b)) => match op {
                Op::Add => Value::Float(a + b),
                Op::Sub => Value::Float(a - b),
                Op::Mul => Value::Float(a * b),
                Op::Div => Value::Float(a / b),
                Op::Mod => Value::Float(a % b),
                _ => unreachable!(),
            },
            (Value::String(a), Value::String(b)) if op == Op::Add => {
                Value::String(format!("{a}{b}"))
            }
            _ => {
                let op_name = match op {
                    Op::Add => "+",
                    Op::Sub => "-",
                    Op::Mul => "*",
                    Op::Div => "/",
                    Op::Mod => "%",
                    _ => unreachable!(),
                };
                return Err(VmError::new(format!(
                    "cannot apply '{op_name}' to {} and {}",
                    self.type_name(&a),
                    self.type_name(&b)
                )));
            }
        };
        self.push(result);
        Ok(())
    }

    fn compare(&mut self, pred: fn(std::cmp::Ordering) -> bool) -> Result<(), VmError> {
        let b = self.pop();
        let a = self.pop();
        let ordering = match (&a, &b) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => a
                .partial_cmp(b)
                .unwrap_or(std::cmp::Ordering::Equal),
            (Value::String(a), Value::String(b)) => a.cmp(b),
            _ => {
                return Err(VmError::new(format!(
                    "cannot compare {} and {}",
                    self.type_name(&a),
                    self.type_name(&b)
                )));
            }
        };
        self.push(Value::Bool(pred(ordering)));
        Ok(())
    }

    // ── Truthiness ────────────────────────────────────────────────

    fn is_truthy(&self, val: &Value) -> bool {
        match val {
            Value::Bool(b) => *b,
            Value::Unit => false,
            _ => true,
        }
    }

    fn is_falsy(&self, val: &Value) -> bool {
        !self.is_truthy(val)
    }

    // ── Value display ─────────────────────────────────────────────

    fn display_value(&self, val: &Value) -> String {
        format!("{val}")
    }

    fn type_name(&self, val: &Value) -> &'static str {
        match val {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
            Value::String(_) => "String",
            Value::List(_) => "List",
            Value::Map(_) => "Map",
            Value::Set(_) => "Set",
            Value::Tuple(_) => "Tuple",
            Value::Record(..) => "Record",
            Value::Variant(..) => "Variant",
            Value::Closure(_) => "Closure",
            Value::BuiltinFn(_) => "BuiltinFn",
            Value::VariantConstructor(..) => "VariantConstructor",
            Value::RecordDescriptor(_) => "RecordDescriptor",
            Value::PrimitiveDescriptor(_) => "PrimitiveDescriptor",
            Value::Channel(_) => "Channel",
            Value::Handle(_) => "Handle",
            Value::Unit => "Unit",
        }
    }

    // ── Builtin dispatch ──────────────────────────────────────────

    fn dispatch_builtin(
        &mut self,
        name: &str,
        args: &[Value],
    ) -> Result<Value, VmError> {
        match name {
            "println" => {
                match args.len() {
                    0 => println!(),
                    1 => println!("{}", self.display_value(&args[0])),
                    _ => {
                        // Print all args space-separated.
                        let parts: Vec<String> =
                            args.iter().map(|v| self.display_value(v)).collect();
                        println!("{}", parts.join(" "));
                    }
                }
                Ok(Value::Unit)
            }
            "print" => {
                match args.len() {
                    0 => {}
                    1 => print!("{}", self.display_value(&args[0])),
                    _ => {
                        let parts: Vec<String> =
                            args.iter().map(|v| self.display_value(v)).collect();
                        print!("{}", parts.join(" "));
                    }
                }
                Ok(Value::Unit)
            }
            "to_string" => {
                if args.len() != 1 {
                    return Err(VmError::new(
                        "to_string expects exactly 1 argument".to_string(),
                    ));
                }
                Ok(Value::String(self.display_value(&args[0])))
            }
            "type_of" => {
                if args.len() != 1 {
                    return Err(VmError::new(
                        "type_of expects exactly 1 argument".to_string(),
                    ));
                }
                Ok(Value::String(self.type_name(&args[0]).to_string()))
            }
            _ => Err(VmError::new(format!("unknown builtin: {name}"))),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Chunk, Function, Op};
    use crate::lexer::Span;

    /// Helper: build a Function from raw bytecode construction.
    fn make_function(
        build: impl FnOnce(&mut Chunk),
    ) -> Rc<Function> {
        let mut func = Function::new("<test>".to_string(), 0);
        build(&mut func.chunk);
        Rc::new(func)
    }

    fn span() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn test_constant_and_return() {
        let script = make_function(|chunk| {
            let idx = chunk.add_constant(Value::Int(42));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(idx, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_arithmetic_add_int() {
        // 2 + 3 = 5
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(2));
            let b = chunk.add_constant(Value::Int(3));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Add, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(5));
    }

    #[test]
    fn test_arithmetic_expression() {
        // 2 + 3 * 4 = 14 (postfix: 2 3 4 * +)
        let script = make_function(|chunk| {
            let two = chunk.add_constant(Value::Int(2));
            let three = chunk.add_constant(Value::Int(3));
            let four = chunk.add_constant(Value::Int(4));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(two, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(three, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(four, span());
            chunk.emit_op(Op::Mul, span());
            chunk.emit_op(Op::Add, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(14));
    }

    #[test]
    fn test_float_arithmetic() {
        // 1.5 + 2.5 = 4.0
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Float(1.5));
            let b = chunk.add_constant(Value::Float(2.5));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Add, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Float(4.0));
    }

    #[test]
    fn test_negate() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(10));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Negate, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(-10));
    }

    #[test]
    fn test_comparison() {
        // 3 < 5 = true
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(3));
            let b = chunk.add_constant(Value::Int(5));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Lt, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_boolean_not() {
        let script = make_function(|chunk| {
            chunk.emit_op(Op::True, span());
            chunk.emit_op(Op::Not, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn test_globals() {
        // Set global "x" = 42, then get it back.
        let script = make_function(|chunk| {
            let name = chunk.add_constant(Value::String("x".to_string()));
            let val = chunk.add_constant(Value::Int(42));
            // Push value
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            // SetGlobal pops TOS
            chunk.emit_op(Op::SetGlobal, span());
            chunk.emit_u16(name, span());
            // GetGlobal pushes it back
            chunk.emit_op(Op::GetGlobal, span());
            chunk.emit_u16(name, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_locals() {
        // local slot 0 = 10, get it back
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(10));
            // Push a placeholder to occupy slot 0
            chunk.emit_op(Op::Unit, span());
            // Push the value and set it into slot 0
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::SetLocal, span());
            chunk.emit_u16(0, span());
            // Pop the SetLocal peek result
            chunk.emit_op(Op::Pop, span());
            // Get local 0
            chunk.emit_op(Op::GetLocal, span());
            chunk.emit_u16(0, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_string_concat() {
        // "hello" + " " + "world"
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::String("hello".to_string()));
            let b = chunk.add_constant(Value::String(" ".to_string()));
            let c = chunk.add_constant(Value::String("world".to_string()));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(c, span());
            chunk.emit_op(Op::StringConcat, span());
            chunk.emit_u8(3, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::String("hello world".to_string()));
    }

    #[test]
    fn test_display_value() {
        // DisplayValue on Int(42) -> "42"
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(42));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::DisplayValue, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::String("42".to_string()));
    }

    #[test]
    fn test_jump_if_false() {
        // if false then 1 else 2 => 2
        let script = make_function(|chunk| {
            let one = chunk.add_constant(Value::Int(1));
            let two = chunk.add_constant(Value::Int(2));
            // Push false
            chunk.emit_op(Op::False, span());
            // JumpIfFalse -> skip the "then" branch
            let patch = chunk.emit_jump(Op::JumpIfFalse, span());
            // Then: push 1, jump over else
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(one, span());
            let skip_else = chunk.emit_jump(Op::Jump, span());
            // Else: push 2
            chunk.patch_jump(patch);
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(two, span());
            chunk.patch_jump(skip_else);
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(2));
    }

    #[test]
    fn test_builtin_println() {
        // CallBuiltin "println" with arg 42
        let script = make_function(|chunk| {
            let name = chunk.add_constant(Value::String("println".to_string()));
            let val = chunk.add_constant(Value::Int(42));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::CallBuiltin, span());
            chunk.emit_u16(name, span());
            chunk.emit_u8(1, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Unit);
    }

    #[test]
    fn test_make_tuple() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(1));
            let b = chunk.add_constant(Value::Int(2));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::MakeTuple, span());
            chunk.emit_u8(2, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Tuple(vec![Value::Int(1), Value::Int(2)]));
    }

    #[test]
    fn test_make_list() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(10));
            let b = chunk.add_constant(Value::Int(20));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::MakeList, span());
            chunk.emit_u16(2, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(
            result,
            Value::List(Rc::new(vec![Value::Int(10), Value::Int(20)]))
        );
    }

    #[test]
    fn test_division_by_zero() {
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(10));
            let b = chunk.add_constant(Value::Int(0));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Div, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("division by zero"));
    }

    #[test]
    fn test_unit_and_pop() {
        // Push Unit, Pop it, push 99, return.
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(99));
            chunk.emit_op(Op::Unit, span());
            chunk.emit_op(Op::Pop, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(99));
    }

    #[test]
    fn test_dup() {
        // Push 5, dup, add => 10
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(5));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::Dup, span());
            chunk.emit_op(Op::Add, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_eq_neq() {
        // 5 == 5 => true
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(5));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::Eq, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        assert_eq!(vm.run(script).unwrap(), Value::Bool(true));

        // 5 != 3 => true
        let script2 = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(5));
            let b = chunk.add_constant(Value::Int(3));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Neq, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm2 = Vm::new();
        assert_eq!(vm2.run(script2).unwrap(), Value::Bool(true));
    }

    #[test]
    fn test_popn() {
        // Push 3 values, PopN 2, return the remaining one.
        let script = make_function(|chunk| {
            let a = chunk.add_constant(Value::Int(1));
            let b = chunk.add_constant(Value::Int(2));
            let c = chunk.add_constant(Value::Int(3));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(a, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(b, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(c, span());
            chunk.emit_op(Op::PopN, span());
            chunk.emit_u8(2, span());
            chunk.emit_op(Op::Return, span());
        });
        let mut vm = Vm::new();
        let result = vm.run(script).unwrap();
        assert_eq!(result, Value::Int(1));
    }
}
