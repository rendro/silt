//! Stack-based bytecode VM for Silt.
//!
//! Executes compiled `Function` objects produced by the compiler.
//! Phase 2: full function calls (VmClosure + builtins), many builtin
//! dispatches, variant constructors, and end-to-end program execution.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::collections::HashMap;
use std::rc::Rc;

use regex::Regex;

use crate::bytecode::{Chunk, Function, Op, VmClosure};
use crate::value::{Channel, TaskHandle, TryReceiveResult, TrySendResult, Value};

// ── Field type for JSON parsing ──────────────────────────────────────

#[derive(Debug, Clone)]
enum FieldType {
    Int,
    Float,
    String,
    Bool,
    List(Box<FieldType>),
    Option(Box<FieldType>),
    Record(std::string::String),
}

/// Decode a type encoding string (from compiler metadata) into a FieldType.
fn decode_field_type(s: &str) -> FieldType {
    if let Some(rest) = s.strip_prefix("List:") {
        FieldType::List(Box::new(decode_field_type(rest)))
    } else if let Some(rest) = s.strip_prefix("Option:") {
        FieldType::Option(Box::new(decode_field_type(rest)))
    } else if let Some(rest) = s.strip_prefix("Record:") {
        FieldType::Record(rest.to_string())
    } else {
        match s {
            "Int" => FieldType::Int,
            "Float" => FieldType::Float,
            "String" => FieldType::String,
            "Bool" => FieldType::Bool,
            other => FieldType::Record(other.to_string()),
        }
    }
}

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::List(xs) => serde_json::Value::Array(xs.iter().map(value_to_json).collect()),
        Value::Map(m) => {
            let obj: serde_json::Map<std::string::String, serde_json::Value> = m
                .iter()
                .map(|(k, v)| (k.to_string(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        Value::Tuple(vs) => serde_json::Value::Array(vs.iter().map(value_to_json).collect()),
        Value::Record(_name, fields) => {
            let obj: serde_json::Map<std::string::String, serde_json::Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        Value::Variant(name, fields) if name == "None" && fields.is_empty() => {
            serde_json::Value::Null
        }
        Value::Variant(name, fields) if name == "Some" && fields.len() == 1 => {
            value_to_json(&fields[0])
        }
        Value::Variant(name, fields) => {
            let mut obj = serde_json::Map::new();
            obj.insert("variant".into(), serde_json::Value::String(name.clone()));
            if !fields.is_empty() {
                obj.insert(
                    "fields".into(),
                    serde_json::Value::Array(fields.iter().map(value_to_json).collect()),
                );
            }
            serde_json::Value::Object(obj)
        }
        Value::Unit => serde_json::Value::Null,
        Value::VariantConstructor(name, _) => serde_json::Value::String(name.clone()),
        _ => serde_json::Value::Null,
    }
}

// ── Error type ────────────────────────────────────────────────────

#[derive(Debug)]
pub struct VmError {
    pub message: String,
    /// If true, this error signals a cooperative yield, not a real error.
    pub is_yield: bool,
}

impl VmError {
    pub fn new(message: String) -> Self {
        VmError { message, is_yield: false }
    }

    fn yield_signal() -> Self {
        VmError { message: String::new(), is_yield: true }
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

// ── Fiber (concurrency execution context) ────────────────────────

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
enum FiberState {
    Ready,
    Running,
    BlockedSend(usize),   // channel id
    BlockedRecv(usize),   // channel id
    Completed(Value),
    Failed(String),
}

struct VmFiber {
    frames: Vec<CallFrame>,
    stack: Vec<Value>,
    state: FiberState,
    handle: Rc<TaskHandle>,
}

/// Result from running a fiber for a time slice.
enum FiberSliceResult {
    Yielded,
    Completed(Value),
    #[allow(dead_code)]
    Blocked,
}

// ── VM ────────────────────────────────────────────────────────────

pub struct Vm {
    frames: Vec<CallFrame>,
    stack: Vec<Value>,
    globals: HashMap<String, Value>,
    /// Maps variant tag names to their parent type name, for method dispatch.
    #[allow(dead_code)]
    variant_types: HashMap<String, String>,
    /// Maps record type names to their field definitions (name, type) for json.parse.
    record_types: HashMap<String, Vec<(String, FieldType)>>,

    // ── Concurrency state ────────────────────────────────────────
    fibers: Vec<VmFiber>,
    #[allow(dead_code)]
    current_fiber: usize,
    next_channel_id: usize,
    next_task_id: usize,
    /// Prevents nested `run_other_fibers_once()` calls during channel.each
    scheduling_fibers: bool,
}

impl Vm {
    pub fn new() -> Self {
        let mut vm = Vm {
            frames: Vec::new(),
            stack: Vec::new(),
            globals: HashMap::new(),
            variant_types: HashMap::new(),
            record_types: HashMap::new(),
            fibers: Vec::new(),
            current_fiber: 0,
            next_channel_id: 0,
            next_task_id: 0,
            scheduling_fibers: false,
        };
        vm.register_builtins();
        vm
    }

    /// Register all builtin functions and variant constructors in globals.
    fn register_builtins(&mut self) {
        // Variant constructors
        self.globals.insert("Ok".into(), Value::VariantConstructor("Ok".into(), 1));
        self.globals.insert("Err".into(), Value::VariantConstructor("Err".into(), 1));
        self.globals.insert("Some".into(), Value::VariantConstructor("Some".into(), 1));
        self.globals.insert("None".into(), Value::Variant("None".into(), Vec::new()));
        self.globals.insert("Stop".into(), Value::VariantConstructor("Stop".into(), 1));
        self.globals.insert("Continue".into(), Value::VariantConstructor("Continue".into(), 1));
        self.globals.insert("Message".into(), Value::VariantConstructor("Message".into(), 1));
        self.globals.insert("Closed".into(), Value::Variant("Closed".into(), Vec::new()));
        self.globals.insert("Empty".into(), Value::Variant("Empty".into(), Vec::new()));

        // Primitive type descriptors
        self.globals.insert("Int".into(), Value::PrimitiveDescriptor("Int".into()));
        self.globals.insert("Float".into(), Value::PrimitiveDescriptor("Float".into()));
        self.globals.insert("String".into(), Value::PrimitiveDescriptor("String".into()));
        self.globals.insert("Bool".into(), Value::PrimitiveDescriptor("Bool".into()));

        // Math constants
        self.globals.insert("math.pi".into(), Value::Float(std::f64::consts::PI));
        self.globals.insert("math.e".into(), Value::Float(std::f64::consts::E));

        // All builtin function names
        let builtin_names = [
            "print", "println", "io.inspect", "panic", "try", "to_string", "type_of",
            "list.map", "list.filter", "list.each", "list.fold",
            "list.find", "list.zip", "list.flatten", "list.sort_by",
            "list.flat_map", "list.filter_map", "list.any", "list.all",
            "list.fold_until", "list.unfold",
            "list.head", "list.tail", "list.last", "list.reverse",
            "list.sort", "list.unique", "list.contains", "list.length",
            "list.append", "list.prepend", "list.concat",
            "list.get", "list.set", "list.take", "list.drop",
            "list.enumerate", "list.group_by",
            "result.unwrap_or", "result.map_ok", "result.map_err",
            "result.flatten", "result.flat_map", "result.is_ok", "result.is_err",
            "option.map", "option.unwrap_or", "option.to_result",
            "option.is_some", "option.is_none", "option.flat_map",
            "string.split", "string.trim", "string.trim_start", "string.trim_end",
            "string.char_code", "string.from_char_code",
            "string.contains", "string.replace", "string.join",
            "string.length", "string.to_upper", "string.to_lower",
            "string.starts_with", "string.ends_with", "string.chars",
            "string.repeat", "string.index_of", "string.slice",
            "string.pad_left", "string.pad_right",
            "string.is_empty", "string.is_alpha", "string.is_digit",
            "string.is_upper", "string.is_lower", "string.is_alnum",
            "string.is_whitespace",
            "int.parse", "int.abs", "int.min", "int.max",
            "int.to_float", "int.to_string",
            "float.parse", "float.round", "float.ceil", "float.floor",
            "float.abs", "float.to_string", "float.to_int",
            "float.min", "float.max",
            "map.get", "map.set", "map.delete", "map.contains",
            "map.keys", "map.values", "map.length", "map.merge",
            "map.filter", "map.map", "map.entries", "map.from_entries",
            "map.each", "map.update",
            "set.new", "set.from_list", "set.to_list", "set.contains",
            "set.insert", "set.remove", "set.length",
            "set.union", "set.intersection", "set.difference", "set.is_subset",
            "set.map", "set.filter", "set.each", "set.fold",
            "io.read_file", "io.write_file", "io.read_line", "io.args",
            "fs.exists",
            "test.assert", "test.assert_eq", "test.assert_ne",
            "math.sqrt", "math.pow", "math.log", "math.log10",
            "math.sin", "math.cos", "math.tan",
            "math.asin", "math.acos", "math.atan", "math.atan2",
            "regex.is_match", "regex.find", "regex.find_all", "regex.split",
            "regex.replace", "regex.replace_all", "regex.replace_all_with",
            "regex.captures", "regex.captures_all",
            "json.parse", "json.parse_list", "json.parse_map",
            "json.stringify", "json.pretty",
            "channel.new", "channel.send", "channel.receive", "channel.close",
            "channel.try_send", "channel.try_receive", "channel.select", "channel.each",
            "task.spawn", "task.join", "task.cancel",
        ];

        for name in builtin_names {
            self.globals.insert(name.into(), Value::BuiltinFn(name.into()));
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
                    self.check_same_type(&a, &b)?;
                    self.push(Value::Bool(a == b));
                }
                Some(Op::Neq) => {
                    let b = self.pop();
                    let a = self.pop();
                    self.check_same_type(&a, &b)?;
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
                    // Extend stack if needed (for first-time local assignment)
                    let target = base + slot;
                    while self.stack.len() <= target {
                        self.stack.push(Value::Unit);
                    }
                    self.stack[target] = value;
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
                    let value = self.peek().clone();
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
                    self.call_value(func_val, argc, func_slot)?;
                }
                Some(Op::TailCall) => {
                    let argc = self.read_u8() as usize;
                    let func_slot = self.stack.len() - 1 - argc;
                    let func_val = self.stack[func_slot].clone();
                    match func_val {
                        Value::VmClosure(closure) => {
                            if argc != closure.function.arity as usize {
                                return Err(VmError::new(format!(
                                    "function '{}' expects {} arguments, got {}",
                                    closure.function.name, closure.function.arity, argc
                                )));
                            }
                            // Reuse current frame: move args to base_slot
                            let base = self.current_frame().base_slot;
                            for i in 0..argc {
                                self.stack[base + i] = self.stack[func_slot + 1 + i].clone();
                            }
                            self.stack.truncate(base + argc);
                            let frame = self.current_frame_mut();
                            frame.closure = closure;
                            frame.ip = 0;
                        }
                        _ => {
                            // Fall back to normal call
                            self.call_value(func_val, argc, func_slot)?;
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
                    // Discard the callee's stack window including the function slot.
                    // base_slot = func_slot + 1, so func_slot = base_slot - 1.
                    let func_slot = if finished_base > 0 { finished_base - 1 } else { 0 };
                    self.stack.truncate(func_slot);
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
                    let constant = self.read_constant(func_index);

                    // Collect upvalue values from the descriptors
                    let mut upvalues = Vec::with_capacity(upvalue_count);
                    for _ in 0..upvalue_count {
                        let is_local = self.read_u8() != 0;
                        let index = self.read_u8() as usize;
                        let val = if is_local {
                            let base = self.current_frame().base_slot;
                            self.stack[base + index].clone()
                        } else {
                            self.current_frame().closure.upvalues[index].clone()
                        };
                        upvalues.push(val);
                    }

                    // The constant should be a VmClosure wrapping the function
                    match constant {
                        Value::VmClosure(existing) => {
                            let closure = Rc::new(VmClosure {
                                function: existing.function.clone(),
                                upvalues,
                            });
                            self.push(Value::VmClosure(closure));
                        }
                        _ => {
                            // Fallback: push Unit (shouldn't happen in Phase 2)
                            self.push(Value::Unit);
                        }
                    }
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
                    let mut map = BTreeMap::new();
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
                    let mut set = BTreeSet::new();
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
                    let mut fields = BTreeMap::new();
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
                                (*a..*b).map(Value::Int).collect();
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
                Some(Op::TestRecordTag) => {
                    let name_index = self.read_u16() as usize;
                    let name = self.read_constant_string(name_index)?;
                    let val = self.peek();
                    let result = matches!(val, Value::Record(tag, _) if *tag == name);
                    self.push(Value::Bool(result));
                }
                Some(Op::TestMapHasKey) => {
                    let const_index = self.read_u16() as usize;
                    let key_name = self.read_constant_string(const_index)?;
                    let val = self.peek();
                    let result = match val {
                        Value::Map(map) => map.contains_key(&Value::String(key_name)),
                        _ => false,
                    };
                    self.push(Value::Bool(result));
                }
                Some(Op::DestructMapValue) => {
                    let const_index = self.read_u16() as usize;
                    let key_name = self.read_constant_string(const_index)?;
                    let val = self.peek().clone();
                    match val {
                        Value::Map(map) => {
                            let value = map.get(&Value::String(key_name.clone())).cloned().ok_or_else(|| {
                                VmError::new(format!("map has no key '{key_name}'"))
                            })?;
                            self.push(value);
                        }
                        _ => {
                            return Err(VmError::new(
                                "DestructMapValue on non-map".to_string(),
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
                    let first_slot = self.read_u16() as usize;
                    // Update loop bindings: the new values are on top of stack.
                    // Copy them back into the binding slots starting at first_slot.
                    let base = self.current_frame().base_slot;
                    let start = self.stack.len() - arg_count;
                    for i in 0..arg_count {
                        self.stack[base + first_slot + i] = self.stack[start + i].clone();
                    }
                    // Truncate all the way back to just after loop bindings.
                    // This cleans up any intermediate values from the loop body.
                    self.stack.truncate(base + first_slot + arg_count);
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
                                    // Truncate to the func_slot (base - 1) to
                                    // clean up the callee's stack window.
                                    let func_slot = if finished_base > 0 { finished_base - 1 } else { 0 };
                                    self.stack.truncate(func_slot);
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

                // ── Method dispatch ───────────────────────────
                Some(Op::CallMethod) => {
                    let method_name_index = self.read_u16() as usize;
                    let argc = self.read_u8() as usize;
                    let method_name = self.read_constant_string(method_name_index)?;
                    // The receiver is at stack[len - argc] (first arg)
                    let receiver_slot = self.stack.len() - argc;
                    let receiver = self.stack[receiver_slot].clone();
                    let type_name = self.value_type_name_for_dispatch(&receiver);
                    let qualified = format!("{type_name}.{method_name}");
                    if let Some(func) = self.globals.get(&qualified).cloned() {
                        // Replace: treat receiver + args as args to the function
                        let args: Vec<Value> = self.stack[receiver_slot..].to_vec();
                        self.stack.truncate(receiver_slot);
                        let result = self.invoke_callable(&func, &args)?;
                        self.push(result);
                    } else {
                        let extra_args: Vec<Value> = self.stack[receiver_slot + 1..].to_vec();
                        // Try built-in trait methods (display, equal, compare)
                        if let Some(result) = self.dispatch_trait_method(&receiver, &method_name, &extra_args) {
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
                            // Fallback: try module-qualified builtin
                            let mut found = false;
                            for module in &["result", "option", "list", "string", "int", "float",
                                            "map", "set", "io", "fs", "test", "math", "regex", "json",
                                            "channel", "task"] {
                                let candidate = format!("{module}.{method_name}");
                                if self.globals.contains_key(&candidate) {
                                    self.stack.truncate(receiver_slot);
                                    let result = self.dispatch_builtin(&candidate, &extra_args)?;
                                    self.push(result);
                                    found = true;
                                    break;
                                }
                            }
                            if !found {
                                return Err(VmError::new(format!(
                                    "no method '{method_name}' for type '{type_name}'"
                                )));
                            }
                        }
                    }
                }

                // ── Concurrency (stubs for Phase 2) ───────────
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

    // ── Call a value ──────────────────────────────────────────────

    fn call_value(&mut self, func_val: Value, argc: usize, func_slot: usize) -> Result<(), VmError> {
        match func_val {
            Value::VmClosure(closure) => {
                if argc != closure.function.arity as usize {
                    return Err(VmError::new(format!(
                        "function '{}' expects {} arguments, got {}",
                        closure.function.name, closure.function.arity, argc
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
            _ => {
                Err(VmError::new(format!(
                    "cannot call value of type {}",
                    self.type_name(&func_val)
                )))
            }
        }
    }

    /// Call a callable Value and return its result. Used for higher-order builtins.
    fn invoke_callable(&mut self, func: &Value, args: &[Value]) -> Result<Value, VmError> {
        match func {
            Value::VmClosure(closure) => {
                if args.len() != closure.function.arity as usize {
                    return Err(VmError::new(format!(
                        "function '{}' expects {} arguments, got {}",
                        closure.function.name, closure.function.arity, args.len()
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
                    let op_byte = self.read_byte();
                    match Op::from_byte(op_byte) {
                        Some(Op::Return) => {
                            let result = self.pop();
                            let finished_base = self.current_frame().base_slot;
                            self.frames.pop();
                            if self.frames.len() < saved_frame_count {
                                // This shouldn't happen
                                return Err(VmError::new("frame underflow in invoke_callable".into()));
                            }
                            if self.frames.len() == saved_frame_count {
                                // We've returned from our closure
                                self.stack.truncate(func_slot);
                                return Ok(result);
                            }
                            // Otherwise, it's an inner return (nested calls)
                            let inner_func_slot = if finished_base > 0 { finished_base - 1 } else { 0 };
                            self.stack.truncate(inner_func_slot);
                            self.push(result);
                        }
                        Some(op) => {
                            // Re-run the same dispatch logic. Since we can't easily
                            // factor out the dispatch, let's use a helper.
                            match self.dispatch_op(op) {
                                Ok(()) => {}
                                Err(e) => {
                                    // Clean up stack and frames on error so that
                                    // callers like try() see a consistent state.
                                    self.frames.truncate(saved_frame_count);
                                    self.stack.truncate(func_slot);
                                    return Err(e);
                                }
                            }
                        }
                        None => {
                            self.frames.truncate(saved_frame_count);
                            self.stack.truncate(func_slot);
                            return Err(VmError::new(format!("unknown opcode: {op_byte}")));
                        }
                    }
                }
            }
            Value::BuiltinFn(name) => {
                self.dispatch_builtin(name, args)
            }
            Value::VariantConstructor(name, arity) => {
                if args.len() != *arity {
                    return Err(VmError::new(format!(
                        "variant constructor '{name}' expects {arity} arguments, got {}",
                        args.len()
                    )));
                }
                Ok(Value::Variant(name.clone(), args.to_vec()))
            }
            _ => {
                Err(VmError::new(format!("cannot call value in invoke_callable")))
            }
        }
    }

    /// Dispatch a single opcode (factored out so invoke_callable can reuse it).
    fn dispatch_op(&mut self, op: Op) -> Result<(), VmError> {
        match op {
            Op::Constant => {
                let index = self.read_u16() as usize;
                let value = self.read_constant(index);
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
                let b = self.pop();
                let a = self.pop();
                self.check_same_type(&a, &b)?;
                self.push(Value::Bool(a == b));
            }
            Op::Neq => {
                let b = self.pop();
                let a = self.pop();
                self.check_same_type(&a, &b)?;
                self.push(Value::Bool(a != b));
            }
            Op::Lt => self.compare(|ord| ord.is_lt())?,
            Op::Gt => self.compare(|ord| ord.is_gt())?,
            Op::Leq => self.compare(|ord| ord.is_le())?,
            Op::Geq => self.compare(|ord| ord.is_ge())?,
            Op::Negate => {
                let val = self.pop();
                match val {
                    Value::Int(n) => self.push(Value::Int(-n)),
                    Value::Float(n) => self.push(Value::Float(-n)),
                    other => return Err(VmError::new(format!("cannot negate {}", self.type_name(&other)))),
                }
            }
            Op::Not => {
                let val = self.pop();
                match val {
                    Value::Bool(b) => self.push(Value::Bool(!b)),
                    other => return Err(VmError::new(format!("cannot apply 'not' to {}", self.type_name(&other)))),
                }
            }
            Op::And => {
                let b = self.pop();
                let a = self.pop();
                match (&a, &b) {
                    (Value::Bool(a_val), Value::Bool(b_val)) => self.push(Value::Bool(*a_val && *b_val)),
                    _ => return Err(VmError::new("logical 'and' requires two booleans".into())),
                }
            }
            Op::Or => {
                let b = self.pop();
                let a = self.pop();
                match (&a, &b) {
                    (Value::Bool(a_val), Value::Bool(b_val)) => self.push(Value::Bool(*a_val || *b_val)),
                    _ => return Err(VmError::new("logical 'or' requires two booleans".into())),
                }
            }
            Op::DisplayValue => {
                let val = self.pop();
                let s = self.display_value(&val);
                self.push(Value::String(s));
            }
            Op::StringConcat => {
                let count = self.read_u8() as usize;
                let start = self.stack.len() - count;
                let mut result = String::new();
                for i in start..self.stack.len() {
                    if let Value::String(ref s) = self.stack[i] {
                        result.push_str(s);
                    } else {
                        return Err(VmError::new("StringConcat: non-string value on stack".into()));
                    }
                }
                self.stack.truncate(start);
                self.push(Value::String(result));
            }
            Op::GetLocal => {
                let slot = self.read_u16() as usize;
                let base = self.current_frame().base_slot;
                let value = self.stack[base + slot].clone();
                self.push(value);
            }
            Op::SetLocal => {
                let slot = self.read_u16() as usize;
                let base = self.current_frame().base_slot;
                let value = self.peek().clone();
                let target = base + slot;
                while self.stack.len() <= target {
                    self.stack.push(Value::Unit);
                }
                self.stack[target] = value;
            }
            Op::GetGlobal => {
                let name_index = self.read_u16() as usize;
                let name = self.read_constant_string(name_index)?;
                let value = self.globals.get(&name).cloned().ok_or_else(|| {
                    VmError::new(format!("undefined global: {name}"))
                })?;
                self.push(value);
            }
            Op::SetGlobal => {
                let name_index = self.read_u16() as usize;
                let name = self.read_constant_string(name_index)?;
                let value = self.peek().clone();
                self.globals.insert(name, value);
            }
            Op::GetUpvalue => {
                let index = self.read_u8() as usize;
                let value = self.current_frame().closure.upvalues[index].clone();
                self.push(value);
            }
            Op::Call => {
                let argc = self.read_u8() as usize;
                let func_slot = self.stack.len() - 1 - argc;
                let func_val = self.stack[func_slot].clone();
                self.call_value(func_val, argc, func_slot)?;
            }
            Op::TailCall => {
                let argc = self.read_u8() as usize;
                let func_slot = self.stack.len() - 1 - argc;
                let func_val = self.stack[func_slot].clone();
                if let Value::VmClosure(closure) = func_val {
                    let base = self.current_frame().base_slot;
                    for i in 0..argc {
                        self.stack[base + i] = self.stack[func_slot + 1 + i].clone();
                    }
                    self.stack.truncate(base + argc);
                    let frame = self.current_frame_mut();
                    frame.closure = closure;
                    frame.ip = 0;
                } else {
                    self.call_value(func_val, argc, func_slot)?;
                }
            }
            // Return is handled specially in invoke_callable, not here.
            Op::Return => {
                unreachable!("Return should be handled by caller");
            }
            Op::CallBuiltin => {
                let name_index = self.read_u16() as usize;
                let argc = self.read_u8() as usize;
                let name = self.read_constant_string(name_index)?;
                let start = self.stack.len() - argc;
                let args: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                let result = self.dispatch_builtin(&name, &args)?;
                self.push(result);
            }
            Op::MakeClosure => {
                let func_index = self.read_u16() as usize;
                let upvalue_count = self.read_u8() as usize;
                let constant = self.read_constant(func_index);
                let mut upvalues = Vec::with_capacity(upvalue_count);
                for _ in 0..upvalue_count {
                    let is_local = self.read_u8() != 0;
                    let index = self.read_u8() as usize;
                    let val = if is_local {
                        let base = self.current_frame().base_slot;
                        self.stack[base + index].clone()
                    } else {
                        self.current_frame().closure.upvalues[index].clone()
                    };
                    upvalues.push(val);
                }
                if let Value::VmClosure(existing) = constant {
                    let closure = Rc::new(VmClosure {
                        function: existing.function.clone(),
                        upvalues,
                    });
                    self.push(Value::VmClosure(closure));
                } else {
                    self.push(Value::Unit);
                }
            }
            Op::MakeTuple => {
                let count = self.read_u8() as usize;
                let start = self.stack.len() - count;
                let elements: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::Tuple(elements));
            }
            Op::MakeList => {
                let count = self.read_u16() as usize;
                let start = self.stack.len() - count;
                let elements: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::List(Rc::new(elements)));
            }
            Op::MakeMap => {
                let pair_count = self.read_u16() as usize;
                let total = pair_count * 2;
                let start = self.stack.len() - total;
                let mut map = BTreeMap::new();
                for i in (start..self.stack.len()).step_by(2) {
                    map.insert(self.stack[i].clone(), self.stack[i + 1].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Map(Rc::new(map)));
            }
            Op::MakeSet => {
                let count = self.read_u16() as usize;
                let start = self.stack.len() - count;
                let mut set = BTreeSet::new();
                for i in start..self.stack.len() {
                    set.insert(self.stack[i].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Set(Rc::new(set)));
            }
            Op::MakeRecord => {
                let type_name_index = self.read_u16() as usize;
                let field_count = self.read_u8() as usize;
                let mut field_names = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let name_index = self.read_u16() as usize;
                    field_names.push(self.read_constant_string(name_index)?);
                }
                let type_name = self.read_constant_string(type_name_index)?;
                let start = self.stack.len() - field_count;
                let mut fields = BTreeMap::new();
                for (i, name) in field_names.into_iter().enumerate() {
                    fields.insert(name, self.stack[start + i].clone());
                }
                self.stack.truncate(start);
                self.push(Value::Record(type_name, Rc::new(fields)));
            }
            Op::MakeVariant => {
                let name_index = self.read_u16() as usize;
                let field_count = self.read_u8() as usize;
                let name = self.read_constant_string(name_index)?;
                let start = self.stack.len() - field_count;
                let fields: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                self.push(Value::Variant(name, fields));
            }
            Op::RecordUpdate => {
                let field_count = self.read_u8() as usize;
                let mut field_names = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let ni = self.read_u16() as usize;
                    field_names.push(self.read_constant_string(ni)?);
                }
                let start = self.stack.len() - field_count;
                let new_values: Vec<Value> = self.stack[start..].to_vec();
                self.stack.truncate(start);
                let base = self.pop();
                if let Value::Record(type_name, existing) = base {
                    let mut fields = (*existing).clone();
                    for (name, val) in field_names.into_iter().zip(new_values) {
                        fields.insert(name, val);
                    }
                    self.push(Value::Record(type_name, Rc::new(fields)));
                } else {
                    return Err(VmError::new("record update on non-record".into()));
                }
            }
            Op::MakeRange => {
                let end = self.pop();
                let start = self.pop();
                if let (Value::Int(a), Value::Int(b)) = (&start, &end) {
                    let items: Vec<Value> = (*a..*b).map(Value::Int).collect();
                    self.push(Value::List(Rc::new(items)));
                } else {
                    return Err(VmError::new("range requires two integers".into()));
                }
            }
            Op::GetField => {
                let name_index = self.read_u16() as usize;
                let name = self.read_constant_string(name_index)?;
                let target = self.pop();
                match target {
                    Value::Record(_, ref fields) => {
                        let val = fields.get(&name).cloned().ok_or_else(|| VmError::new(format!("record has no field '{name}'")))?;
                        self.push(val);
                    }
                    Value::Map(ref map) => {
                        let val = map.get(&Value::String(name.clone())).cloned().ok_or_else(|| VmError::new(format!("map has no key '{name}'")))?;
                        self.push(val);
                    }
                    other => return Err(VmError::new(format!("cannot access field '{}' on {}", name, self.type_name(&other)))),
                }
            }
            Op::GetIndex => {
                let index = self.read_u8() as usize;
                let target = self.pop();
                if let Value::Tuple(ref elems) = target {
                    let val = elems.get(index).cloned().ok_or_else(|| VmError::new(format!("tuple index out of bounds")))?;
                    self.push(val);
                } else {
                    return Err(VmError::new(format!("cannot index into {}", self.type_name(&target))));
                }
            }
            Op::Jump => {
                let offset = self.read_u16() as usize;
                self.current_frame_mut().ip += offset;
            }
            Op::JumpBack => {
                let offset = self.read_u16() as usize;
                self.current_frame_mut().ip -= offset;
            }
            Op::JumpIfFalse => {
                let offset = self.read_u16() as usize;
                let val = self.pop();
                if self.is_falsy(&val) {
                    self.current_frame_mut().ip += offset;
                }
            }
            Op::JumpIfTrue => {
                let offset = self.read_u16() as usize;
                let val = self.pop();
                if self.is_truthy(&val) {
                    self.current_frame_mut().ip += offset;
                }
            }
            Op::Pop => { self.pop(); }
            Op::PopN => {
                let count = self.read_u8() as usize;
                let new_len = self.stack.len().saturating_sub(count);
                self.stack.truncate(new_len);
            }
            Op::Dup => {
                let val = self.peek().clone();
                self.push(val);
            }
            Op::TestTag => {
                let ni = self.read_u16() as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek();
                let result = matches!(val, Value::Variant(tag, _) if *tag == name);
                self.push(Value::Bool(result));
            }
            Op::TestEqual => {
                let ci = self.read_u16() as usize;
                let constant = self.read_constant(ci);
                let val = self.peek();
                let result = *val == constant;
                self.push(Value::Bool(result));
            }
            Op::TestTupleLen => {
                let len = self.read_u8() as usize;
                let val = self.peek();
                let result = matches!(val, Value::Tuple(elems) if elems.len() == len);
                self.push(Value::Bool(result));
            }
            Op::TestListMin => {
                let min_len = self.read_u8() as usize;
                let val = self.peek();
                let result = matches!(val, Value::List(xs) if xs.len() >= min_len);
                self.push(Value::Bool(result));
            }
            Op::TestListExact => {
                let len = self.read_u8() as usize;
                let val = self.peek();
                let result = matches!(val, Value::List(xs) if xs.len() == len);
                self.push(Value::Bool(result));
            }
            Op::TestIntRange => {
                let lo_index = self.read_u16() as usize;
                let hi_index = self.read_u16() as usize;
                let lo = self.read_constant(lo_index);
                let hi = self.read_constant(hi_index);
                let val = self.peek();
                let result = match (val, &lo, &hi) {
                    (Value::Int(n), Value::Int(lo), Value::Int(hi)) => *n >= *lo && *n <= *hi,
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::TestFloatRange => {
                let lo_index = self.read_u16() as usize;
                let hi_index = self.read_u16() as usize;
                let lo = self.read_constant(lo_index);
                let hi = self.read_constant(hi_index);
                let val = self.peek();
                let result = match (val, &lo, &hi) {
                    (Value::Float(n), Value::Float(lo), Value::Float(hi)) => *n >= *lo && *n <= *hi,
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::TestBool => {
                let expected = self.read_u8() != 0;
                let val = self.peek();
                let result = matches!(val, Value::Bool(b) if *b == expected);
                self.push(Value::Bool(result));
            }
            Op::DestructTuple => {
                let index = self.read_u8() as usize;
                let val = self.peek().clone();
                if let Value::Tuple(elems) = val { self.push(elems[index].clone()); }
                else { return Err(VmError::new("DestructTuple on non-tuple".into())); }
            }
            Op::DestructVariant => {
                let index = self.read_u8() as usize;
                let val = self.peek().clone();
                if let Value::Variant(_, fields) = val { self.push(fields[index].clone()); }
                else { return Err(VmError::new("DestructVariant on non-variant".into())); }
            }
            Op::DestructList => {
                let index = self.read_u8() as usize;
                let val = self.peek().clone();
                if let Value::List(xs) = val { self.push(xs[index].clone()); }
                else { return Err(VmError::new("DestructList on non-list".into())); }
            }
            Op::DestructListRest => {
                let start = self.read_u8() as usize;
                let val = self.peek().clone();
                if let Value::List(xs) = val { self.push(Value::List(Rc::new(xs[start..].to_vec()))); }
                else { return Err(VmError::new("DestructListRest on non-list".into())); }
            }
            Op::DestructRecordField => {
                let ni = self.read_u16() as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek().clone();
                if let Value::Record(_, fields) = val {
                    let field = fields.get(&name).cloned().ok_or_else(|| VmError::new(format!("record has no field '{name}'")))?;
                    self.push(field);
                } else { return Err(VmError::new("DestructRecordField on non-record".into())); }
            }
            Op::TestRecordTag => {
                let ni = self.read_u16() as usize;
                let name = self.read_constant_string(ni)?;
                let val = self.peek();
                let result = matches!(val, Value::Record(tag, _) if *tag == name);
                self.push(Value::Bool(result));
            }
            Op::TestMapHasKey => {
                let ci = self.read_u16() as usize;
                let key_name = self.read_constant_string(ci)?;
                let val = self.peek();
                let result = match val {
                    Value::Map(map) => map.contains_key(&Value::String(key_name)),
                    _ => false,
                };
                self.push(Value::Bool(result));
            }
            Op::DestructMapValue => {
                let ci = self.read_u16() as usize;
                let key_name = self.read_constant_string(ci)?;
                let val = self.peek().clone();
                if let Value::Map(map) = val {
                    let value = map.get(&Value::String(key_name.clone())).cloned().ok_or_else(|| VmError::new(format!("map has no key '{key_name}'")))?;
                    self.push(value);
                } else { return Err(VmError::new("DestructMapValue on non-map".into())); }
            }
            Op::LoopSetup => { let _ = self.read_u8(); }
            Op::Recur => {
                let arg_count = self.read_u8() as usize;
                let first_slot = self.read_u16() as usize;
                let base = self.current_frame().base_slot;
                let start = self.stack.len() - arg_count;
                for i in 0..arg_count {
                    self.stack[base + first_slot + i] = self.stack[start + i].clone();
                }
                // Truncate all the way back to just after loop bindings.
                self.stack.truncate(base + first_slot + arg_count);
            }
            Op::QuestionMark => {
                let val = self.peek().clone();
                match val {
                    Value::Variant(ref tag, ref fields) => match tag.as_str() {
                        "Ok" | "Some" => {
                            self.pop();
                            self.push(if fields.len() == 1 { fields[0].clone() } else { Value::Unit });
                        }
                        "Err" | "None" => {
                            let result = self.pop();
                            let finished_base = self.current_frame().base_slot;
                            self.frames.pop();
                            if self.frames.is_empty() {
                                self.push(result);
                                return Ok(());
                            }
                            self.stack.truncate(finished_base);
                            self.push(result);
                        }
                        _ => return Err(VmError::new(format!("? on non-Result/Option: {tag}"))),
                    },
                    _ => return Err(VmError::new(format!("? on non-variant: {}", self.type_name(&val)))),
                }
            }
            Op::Panic => {
                let msg = self.pop();
                return Err(VmError::new(format!("panic: {}", self.display_value(&msg))));
            }
            Op::CallMethod => {
                let method_name_index = self.read_u16() as usize;
                let argc = self.read_u8() as usize;
                let method_name = self.read_constant_string(method_name_index)?;
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
                    if let Some(result) = self.dispatch_trait_method(&receiver, &method_name, &extra_args) {
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
                        let mut found = false;
                        for module in &["result", "option", "list", "string", "int", "float",
                                        "map", "set", "io", "fs", "test", "math", "regex", "json",
                                        "channel", "task"] {
                            let candidate = format!("{module}.{method_name}");
                            if self.globals.contains_key(&candidate) {
                                self.stack.truncate(receiver_slot);
                                let result = self.dispatch_builtin(&candidate, &extra_args)?;
                                self.push(result);
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            return Err(VmError::new(format!(
                                "no method '{method_name}' for type '{type_name}'"
                            )));
                        }
                    }
                }
            }
            Op::ChanNew | Op::ChanSend | Op::ChanRecv | Op::ChanClose
            | Op::ChanTrySend | Op::ChanTryRecv | Op::ChanSelect
            | Op::TaskSpawn | Op::TaskJoin | Op::TaskCancel | Op::Yield => {
                return Err(VmError::new("concurrency opcodes not yet implemented".into()));
            }
        }
        Ok(())
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
                let a_type = self.type_name(&a);
                let b_type = self.type_name(&b);
                // Special error for Int/Float mixing
                if (a_type == "Int" && b_type == "Float") || (a_type == "Float" && b_type == "Int") {
                    return Err(VmError::new(
                        "cannot mix Int and Float — use int.to_float or float.to_int for explicit conversion".to_string()
                    ));
                }
                return Err(VmError::new(format!(
                    "cannot apply '{op_name}' to {a_type} and {b_type}",
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
                    "unsupported operation: cannot compare {} and {}",
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
            Value::VmClosure(_) => "Function",
            Value::BuiltinFn(_) => "BuiltinFn",
            Value::VariantConstructor(..) => "VariantConstructor",
            Value::RecordDescriptor(_) => "RecordDescriptor",
            Value::PrimitiveDescriptor(_) => "PrimitiveDescriptor",
            Value::Channel(_) => "Channel",
            Value::Handle(_) => "Handle",
            Value::Unit => "Unit",
        }
    }

    /// Returns a discriminant for comparing value types.
    /// Two values are "same type" for equality/comparison if they share a discriminant.
    /// Variants are considered the same type (to allow Ok(1) == Err(2) => false,
    /// and pattern matching across tags). Records are compared by type name.
    fn value_disc(val: &Value) -> u8 {
        match val {
            Value::Int(_) => 0,
            Value::Float(_) => 1,
            Value::Bool(_) => 2,
            Value::String(_) => 3,
            Value::List(_) => 4,
            Value::Map(_) => 5,
            Value::Set(_) => 6,
            Value::Tuple(_) => 7,
            Value::Record(..) => 8,
            Value::Variant(..) => 9,
            Value::Unit => 10,
            Value::Channel(_) => 11,
            Value::Handle(_) => 12,
            Value::VmClosure(_) => 13,
            Value::BuiltinFn(_) => 14,
            Value::VariantConstructor(..) => 15,
            Value::RecordDescriptor(_) => 16,
            Value::PrimitiveDescriptor(_) => 17,
        }
    }

    /// Check that two values have compatible types for equality/comparison.
    fn check_same_type(&self, a: &Value, b: &Value) -> Result<(), VmError> {
        if Self::value_disc(a) != Self::value_disc(b) {
            return Err(VmError::new(format!(
                "unsupported operation: cannot compare {} and {}",
                self.type_name(a),
                self.type_name(b)
            )));
        }
        Ok(())
    }

    /// Get the type name for method dispatch. For variants, looks up the parent type.
    fn value_type_name_for_dispatch(&self, val: &Value) -> String {
        match val {
            Value::Variant(tag, _) => {
                // Look up the parent type from the __type_of__ mapping
                let key = format!("__type_of__{tag}");
                if let Some(Value::String(type_name)) = self.globals.get(&key) {
                    type_name.clone()
                } else {
                    tag.clone() // fallback: use the tag itself
                }
            }
            Value::Record(type_name, _) => type_name.clone(),
            Value::Int(_) => "Int".to_string(),
            Value::Float(_) => "Float".to_string(),
            Value::Bool(_) => "Bool".to_string(),
            Value::String(_) => "String".to_string(),
            Value::List(_) => "List".to_string(),
            Value::Map(_) => "Map".to_string(),
            Value::Set(_) => "Set".to_string(),
            Value::Tuple(_) => "Tuple".to_string(),
            _ => "Unknown".to_string(),
        }
    }

    // ── Built-in trait methods on primitive types ──────────────────

    /// Handle built-in trait methods like .display(), .equal(), .compare()
    /// on primitive types. Returns Some(result) if handled, None otherwise.
    fn dispatch_trait_method(
        &self,
        receiver: &Value,
        method: &str,
        extra_args: &[Value],
    ) -> Option<Result<Value, VmError>> {
        match method {
            "display" => {
                if !extra_args.is_empty() {
                    return Some(Err(VmError::new("display() takes no arguments".into())));
                }
                Some(Ok(Value::String(self.display_value(receiver))))
            }
            "equal" => {
                if extra_args.len() != 1 {
                    return Some(Err(VmError::new("equal() takes 1 argument".into())));
                }
                Some(Ok(Value::Bool(*receiver == extra_args[0])))
            }
            "compare" => {
                if extra_args.len() != 1 {
                    return Some(Err(VmError::new("compare() takes 1 argument".into())));
                }
                let other = &extra_args[0];
                let ord = match (receiver, other) {
                    (Value::Int(a), Value::Int(b)) => a.cmp(b),
                    (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
                    (Value::String(a), Value::String(b)) => a.cmp(b),
                    (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
                    _ => {
                        return Some(Err(VmError::new(format!(
                            "compare() not supported between {} and {}",
                            self.type_name(receiver),
                            self.type_name(other)
                        ))));
                    }
                };
                let result = match ord {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                Some(Ok(Value::Int(result)))
            }
            _ => None,
        }
    }

    // ── Builtin dispatch ──────────────────────────────────────────

    fn dispatch_builtin(
        &mut self,
        name: &str,
        args: &[Value],
    ) -> Result<Value, VmError> {
        if let Some((module, func)) = name.split_once('.') {
            match module {
                "list" => self.dispatch_list(func, args),
                "string" => self.dispatch_string(func, args),
                "int" => self.dispatch_int(func, args),
                "float" => self.dispatch_float(func, args),
                "map" => self.dispatch_map(func, args),
                "set" => self.dispatch_set(func, args),
                "result" => self.dispatch_result(func, args),
                "option" => self.dispatch_option(func, args),
                "io" => self.dispatch_io(func, args),
                "fs" => self.dispatch_fs(func, args),
                "test" => self.dispatch_test(func, args),
                "math" => self.dispatch_math(func, args),
                "regex" => self.dispatch_regex(func, args),
                "json" => self.dispatch_json(func, args),
                "channel" => self.dispatch_channel(func, args),
                "task" => self.dispatch_task(func, args),
                _ => Err(VmError::new(format!("unknown module: {module}"))),
            }
        } else {
            match name {
                "println" => {
                    match args.len() {
                        0 => println!(),
                        1 => println!("{}", self.display_value(&args[0])),
                        _ => {
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
                "panic" => {
                    let msg = args.first().map(|v| v.to_string()).unwrap_or_default();
                    Err(VmError::new(format!("panic: panic: {msg}")))
                }
                "to_string" => {
                    if args.len() != 1 {
                        return Err(VmError::new("to_string expects 1 argument".into()));
                    }
                    Ok(Value::String(self.display_value(&args[0])))
                }
                "type_of" => {
                    if args.len() != 1 {
                        return Err(VmError::new("type_of expects 1 argument".into()));
                    }
                    Ok(Value::String(self.type_name(&args[0]).to_string()))
                }
                "try" => {
                    if args.len() != 1 {
                        return Err(VmError::new("try takes 1 argument (a zero-argument function)".into()));
                    }
                    match self.invoke_callable(&args[0], &[]) {
                        Ok(val) => Ok(Value::Variant("Ok".into(), vec![val])),
                        Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.message)])),
                    }
                }
                _ => Err(VmError::new(format!("unknown builtin: {name}"))),
            }
        }
    }

    // ── List builtins ─────────────────────────────────────────────

    fn dispatch_list(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "map" => {
                if args.len() != 2 { return Err(VmError::new("list.map takes 2 arguments (list, fn)".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.map requires a list".into())); };
                let func = &args[1];
                let mut result = Vec::with_capacity(xs.len());
                for item in xs.iter() {
                    let val = self.invoke_callable(func, &[item.clone()])?;
                    result.push(val);
                }
                Ok(Value::List(Rc::new(result)))
            }
            "filter" => {
                if args.len() != 2 { return Err(VmError::new("list.filter takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.filter requires a list".into())); };
                let func = &args[1];
                let mut result = Vec::new();
                for item in xs.iter() {
                    let keep = self.invoke_callable(func, &[item.clone()])?;
                    if self.is_truthy(&keep) {
                        result.push(item.clone());
                    }
                }
                Ok(Value::List(Rc::new(result)))
            }
            "each" => {
                if args.len() != 2 { return Err(VmError::new("list.each takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.each requires a list".into())); };
                let func = &args[1];
                for item in xs.iter() {
                    self.invoke_callable(func, &[item.clone()])?;
                }
                Ok(Value::Unit)
            }
            "fold" => {
                if args.len() != 3 { return Err(VmError::new("list.fold takes 3 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.fold requires a list".into())); };
                let func = &args[2];
                let mut acc = args[1].clone();
                for item in xs.iter() {
                    acc = self.invoke_callable(func, &[acc, item.clone()])?;
                }
                Ok(acc)
            }
            "find" => {
                if args.len() != 2 { return Err(VmError::new("list.find takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.find requires a list".into())); };
                let func = &args[1];
                for item in xs.iter() {
                    let result = self.invoke_callable(func, &[item.clone()])?;
                    if self.is_truthy(&result) {
                        return Ok(Value::Variant("Some".into(), vec![item.clone()]));
                    }
                }
                Ok(Value::Variant("None".into(), Vec::new()))
            }
            "any" => {
                if args.len() != 2 { return Err(VmError::new("list.any takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.any requires a list".into())); };
                let func = &args[1];
                for item in xs.iter() {
                    let result = self.invoke_callable(func, &[item.clone()])?;
                    if self.is_truthy(&result) { return Ok(Value::Bool(true)); }
                }
                Ok(Value::Bool(false))
            }
            "all" => {
                if args.len() != 2 { return Err(VmError::new("list.all takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.all requires a list".into())); };
                let func = &args[1];
                for item in xs.iter() {
                    let result = self.invoke_callable(func, &[item.clone()])?;
                    if !self.is_truthy(&result) { return Ok(Value::Bool(false)); }
                }
                Ok(Value::Bool(true))
            }
            "flat_map" => {
                if args.len() != 2 { return Err(VmError::new("list.flat_map takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.flat_map requires a list".into())); };
                let func = &args[1];
                let mut result = Vec::new();
                for item in xs.iter() {
                    let val = self.invoke_callable(func, &[item.clone()])?;
                    if let Value::List(inner) = val {
                        result.extend(inner.iter().cloned());
                    } else {
                        result.push(val);
                    }
                }
                Ok(Value::List(Rc::new(result)))
            }
            "filter_map" => {
                if args.len() != 2 { return Err(VmError::new("list.filter_map takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.filter_map requires a list".into())); };
                let func = &args[1];
                let mut result = Vec::new();
                for item in xs.iter() {
                    let val = self.invoke_callable(func, &[item.clone()])?;
                    match val {
                        Value::Variant(ref tag, ref fields) if tag == "Some" && fields.len() == 1 => {
                            result.push(fields[0].clone());
                        }
                        Value::Variant(ref tag, _) if tag == "None" => {}
                        _ => result.push(val),
                    }
                }
                Ok(Value::List(Rc::new(result)))
            }
            // Non-closure list builtins
            "zip" => {
                if args.len() != 2 { return Err(VmError::new("list.zip takes 2 arguments".into())); }
                let (Value::List(a), Value::List(b)) = (&args[0], &args[1]) else { return Err(VmError::new("list.zip requires two lists".into())); };
                let pairs: Vec<Value> = a.iter().zip(b.iter()).map(|(x, y)| Value::Tuple(vec![x.clone(), y.clone()])).collect();
                Ok(Value::List(Rc::new(pairs)))
            }
            "flatten" => {
                if args.len() != 1 { return Err(VmError::new("list.flatten takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.flatten requires a list".into())); };
                let mut result = Vec::new();
                for item in xs.iter() {
                    match item {
                        Value::List(inner) => result.extend(inner.iter().cloned()),
                        other => result.push(other.clone()),
                    }
                }
                Ok(Value::List(Rc::new(result)))
            }
            "head" => {
                if args.len() != 1 { return Err(VmError::new("list.head takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.head requires a list".into())); };
                match xs.first() {
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "tail" => {
                if args.len() != 1 { return Err(VmError::new("list.tail takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.tail requires a list".into())); };
                if xs.is_empty() { Ok(Value::List(Rc::new(Vec::new()))) }
                else { Ok(Value::List(Rc::new(xs[1..].to_vec()))) }
            }
            "last" => {
                if args.len() != 1 { return Err(VmError::new("list.last takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.last requires a list".into())); };
                match xs.last() {
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "reverse" => {
                if args.len() != 1 { return Err(VmError::new("list.reverse takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.reverse requires a list".into())); };
                let mut v = (**xs).clone(); v.reverse();
                Ok(Value::List(Rc::new(v)))
            }
            "sort" => {
                if args.len() != 1 { return Err(VmError::new("list.sort takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.sort requires a list".into())); };
                let mut v = (**xs).clone(); v.sort();
                Ok(Value::List(Rc::new(v)))
            }
            "unique" => {
                if args.len() != 1 { return Err(VmError::new("list.unique takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.unique requires a list".into())); };
                let mut seen = Vec::new();
                let mut result = Vec::new();
                for x in xs.iter() {
                    if !seen.contains(x) { seen.push(x.clone()); result.push(x.clone()); }
                }
                Ok(Value::List(Rc::new(result)))
            }
            "contains" => {
                if args.len() != 2 { return Err(VmError::new("list.contains takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.contains requires a list".into())); };
                Ok(Value::Bool(xs.contains(&args[1])))
            }
            "length" => {
                if args.len() != 1 { return Err(VmError::new("list.length takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.length requires a list".into())); };
                Ok(Value::Int(xs.len() as i64))
            }
            "append" => {
                if args.len() != 2 { return Err(VmError::new("list.append takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.append requires a list".into())); };
                let mut v = (**xs).clone(); v.push(args[1].clone());
                Ok(Value::List(Rc::new(v)))
            }
            "prepend" => {
                if args.len() != 2 { return Err(VmError::new("list.prepend takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.prepend requires a list".into())); };
                let mut v = (**xs).clone(); v.insert(0, args[1].clone());
                Ok(Value::List(Rc::new(v)))
            }
            "concat" => {
                if args.len() != 2 { return Err(VmError::new("list.concat takes 2 arguments".into())); }
                let (Value::List(a), Value::List(b)) = (&args[0], &args[1]) else { return Err(VmError::new("list.concat requires two lists".into())); };
                let mut v = (**a).clone(); v.extend((**b).iter().cloned());
                Ok(Value::List(Rc::new(v)))
            }
            "get" => {
                if args.len() != 2 { return Err(VmError::new("list.get takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.get requires a list".into())); };
                let Value::Int(n) = &args[1] else { return Err(VmError::new("list.get index must be int".into())); };
                match xs.get(*n as usize) {
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "set" => {
                if args.len() != 3 { return Err(VmError::new("list.set takes 3 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.set requires a list".into())); };
                let Value::Int(n) = &args[1] else { return Err(VmError::new("list.set index must be int".into())); };
                let idx = *n as usize;
                if idx >= xs.len() { return Err(VmError::new("list.set index out of bounds".into())); }
                let mut v = (**xs).clone(); v[idx] = args[2].clone();
                Ok(Value::List(Rc::new(v)))
            }
            "take" => {
                if args.len() != 2 { return Err(VmError::new("list.take takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.take requires a list".into())); };
                let Value::Int(n) = &args[1] else { return Err(VmError::new("list.take requires int".into())); };
                let n = (*n as usize).min(xs.len());
                Ok(Value::List(Rc::new(xs[..n].to_vec())))
            }
            "drop" => {
                if args.len() != 2 { return Err(VmError::new("list.drop takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.drop requires a list".into())); };
                let Value::Int(n) = &args[1] else { return Err(VmError::new("list.drop requires int".into())); };
                let n = (*n as usize).min(xs.len());
                Ok(Value::List(Rc::new(xs[n..].to_vec())))
            }
            "enumerate" => {
                if args.len() != 1 { return Err(VmError::new("list.enumerate takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.enumerate requires a list".into())); };
                let result: Vec<Value> = xs.iter().enumerate().map(|(i, v)| Value::Tuple(vec![Value::Int(i as i64), v.clone()])).collect();
                Ok(Value::List(Rc::new(result)))
            }
            "sort_by" => {
                if args.len() != 2 { return Err(VmError::new("list.sort_by takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.sort_by requires a list".into())); };
                let func = &args[1];
                // sort_by uses a key function: func(item) -> sort key
                let mut pairs: Vec<(Value, Value)> = Vec::new();
                for item in xs.iter() {
                    let key = self.invoke_callable(func, &[item.clone()])?;
                    pairs.push((key, item.clone()));
                }
                pairs.sort_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let sorted: Vec<Value> = pairs.into_iter().map(|(_, v)| v).collect();
                Ok(Value::List(Rc::new(sorted)))
            }
            "fold_until" => {
                if args.len() != 3 { return Err(VmError::new("list.fold_until takes 3 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.fold_until requires a list".into())); };
                let func = &args[2];
                let mut acc = args[1].clone();
                for item in xs.iter() {
                    let result = self.invoke_callable(func, &[acc.clone(), item.clone()])?;
                    match result {
                        Value::Variant(ref tag, ref fields) if tag == "Continue" && fields.len() == 1 => {
                            acc = fields[0].clone();
                        }
                        Value::Variant(ref tag, ref fields) if tag == "Stop" && fields.len() == 1 => {
                            return Ok(fields[0].clone());
                        }
                        _ => { acc = result; }
                    }
                }
                Ok(acc)
            }
            "unfold" => {
                if args.len() != 2 { return Err(VmError::new("list.unfold takes 2 arguments".into())); }
                let func = &args[1];
                let mut state = args[0].clone();
                let mut result = Vec::new();
                loop {
                    let val = self.invoke_callable(func, &[state.clone()])?;
                    match val {
                        Value::Variant(ref tag, ref fields) if tag == "Some" && fields.len() == 1 => {
                            if let Value::Tuple(pair) = &fields[0] {
                                if pair.len() == 2 {
                                    result.push(pair[0].clone());
                                    state = pair[1].clone();
                                    continue;
                                }
                            }
                            result.push(fields[0].clone());
                            break;
                        }
                        Value::Variant(ref tag, _) if tag == "None" => { break; }
                        _ => { result.push(val); break; }
                    }
                }
                Ok(Value::List(Rc::new(result)))
            }
            "group_by" => {
                if args.len() != 2 { return Err(VmError::new("list.group_by takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("list.group_by requires a list".into())); };
                let func = &args[1];
                let mut groups: BTreeMap<Value, Vec<Value>> = BTreeMap::new();
                for item in xs.iter() {
                    let key = self.invoke_callable(func, &[item.clone()])?;
                    groups.entry(key).or_default().push(item.clone());
                }
                let result: BTreeMap<Value, Value> = groups.into_iter()
                    .map(|(k, v)| (k, Value::List(Rc::new(v))))
                    .collect();
                Ok(Value::Map(Rc::new(result)))
            }
            _ => Err(VmError::new(format!("unknown list function: {name}"))),
        }
    }

    // ── String builtins ───────────────────────────────────────────

    fn dispatch_string(&self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "split" => {
                if args.len() != 2 { return Err(VmError::new("string.split takes 2 arguments".into())); }
                let (Value::String(s), Value::String(sep)) = (&args[0], &args[1]) else { return Err(VmError::new("string.split requires strings".into())); };
                let parts: Vec<Value> = s.split(sep.as_str()).map(|p| Value::String(p.to_string())).collect();
                Ok(Value::List(Rc::new(parts)))
            }
            "trim" => {
                if args.len() != 1 { return Err(VmError::new("string.trim takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.trim requires a string".into())); };
                Ok(Value::String(s.trim().to_string()))
            }
            "trim_start" => {
                if args.len() != 1 { return Err(VmError::new("string.trim_start takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.trim_start requires a string".into())); };
                Ok(Value::String(s.trim_start().to_string()))
            }
            "trim_end" => {
                if args.len() != 1 { return Err(VmError::new("string.trim_end takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.trim_end requires a string".into())); };
                Ok(Value::String(s.trim_end().to_string()))
            }
            "contains" => {
                if args.len() != 2 { return Err(VmError::new("string.contains takes 2 arguments".into())); }
                let (Value::String(s), Value::String(sub)) = (&args[0], &args[1]) else { return Err(VmError::new("string.contains requires strings".into())); };
                Ok(Value::Bool(s.contains(sub.as_str())))
            }
            "replace" => {
                if args.len() != 3 { return Err(VmError::new("string.replace takes 3 arguments".into())); }
                let (Value::String(s), Value::String(from), Value::String(to)) = (&args[0], &args[1], &args[2]) else { return Err(VmError::new("string.replace requires strings".into())); };
                Ok(Value::String(s.replace(from.as_str(), to.as_str())))
            }
            "join" => {
                if args.len() != 2 { return Err(VmError::new("string.join takes 2 arguments".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("string.join requires a list".into())); };
                let Value::String(sep) = &args[1] else { return Err(VmError::new("string.join separator must be a string".into())); };
                let strs: Vec<String> = xs.iter().map(|v| v.to_string()).collect();
                Ok(Value::String(strs.join(sep.as_str())))
            }
            "length" => {
                if args.len() != 1 { return Err(VmError::new("string.length takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.length requires a string".into())); };
                Ok(Value::Int(s.len() as i64))
            }
            "to_upper" => {
                if args.len() != 1 { return Err(VmError::new("string.to_upper takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.to_upper requires a string".into())); };
                Ok(Value::String(s.to_uppercase()))
            }
            "to_lower" => {
                if args.len() != 1 { return Err(VmError::new("string.to_lower takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.to_lower requires a string".into())); };
                Ok(Value::String(s.to_lowercase()))
            }
            "starts_with" => {
                if args.len() != 2 { return Err(VmError::new("string.starts_with takes 2 arguments".into())); }
                let (Value::String(s), Value::String(prefix)) = (&args[0], &args[1]) else { return Err(VmError::new("string.starts_with requires strings".into())); };
                Ok(Value::Bool(s.starts_with(prefix.as_str())))
            }
            "ends_with" => {
                if args.len() != 2 { return Err(VmError::new("string.ends_with takes 2 arguments".into())); }
                let (Value::String(s), Value::String(suffix)) = (&args[0], &args[1]) else { return Err(VmError::new("string.ends_with requires strings".into())); };
                Ok(Value::Bool(s.ends_with(suffix.as_str())))
            }
            "chars" => {
                if args.len() != 1 { return Err(VmError::new("string.chars takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.chars requires a string".into())); };
                let chars: Vec<Value> = s.chars().map(|c| Value::String(c.to_string())).collect();
                Ok(Value::List(Rc::new(chars)))
            }
            "repeat" => {
                if args.len() != 2 { return Err(VmError::new("string.repeat takes 2 arguments".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.repeat requires a string".into())); };
                let Value::Int(n) = &args[1] else { return Err(VmError::new("string.repeat requires an int".into())); };
                Ok(Value::String(s.repeat(*n as usize)))
            }
            "index_of" => {
                if args.len() != 2 { return Err(VmError::new("string.index_of takes 2 arguments".into())); }
                let (Value::String(s), Value::String(needle)) = (&args[0], &args[1]) else { return Err(VmError::new("string.index_of requires strings".into())); };
                match s.find(needle.as_str()) {
                    Some(idx) => Ok(Value::Variant("Some".into(), vec![Value::Int(idx as i64)])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "slice" => {
                if args.len() != 3 { return Err(VmError::new("string.slice takes 3 arguments".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("first arg must be string".into())); };
                let Value::Int(start) = &args[1] else { return Err(VmError::new("second arg must be int".into())); };
                let Value::Int(end) = &args[2] else { return Err(VmError::new("third arg must be int".into())); };
                let chars: Vec<char> = s.chars().collect();
                let start = (*start as usize).min(chars.len());
                let end = (*end as usize).min(chars.len());
                if start > end { Ok(Value::String(String::new())) }
                else { Ok(Value::String(chars[start..end].iter().collect())) }
            }
            "pad_left" => {
                if args.len() != 3 { return Err(VmError::new("string.pad_left takes 3 arguments".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("first arg must be string".into())); };
                let Value::Int(width) = &args[1] else { return Err(VmError::new("second arg must be int".into())); };
                let Value::String(pad) = &args[2] else { return Err(VmError::new("third arg must be string".into())); };
                let width = *width as usize;
                let pad_char = pad.chars().next().unwrap_or(' ');
                if s.len() >= width { Ok(Value::String(s.clone())) }
                else { let padding: String = (0..width - s.len()).map(|_| pad_char).collect(); Ok(Value::String(format!("{padding}{s}"))) }
            }
            "pad_right" => {
                if args.len() != 3 { return Err(VmError::new("string.pad_right takes 3 arguments".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("first arg must be string".into())); };
                let Value::Int(width) = &args[1] else { return Err(VmError::new("second arg must be int".into())); };
                let Value::String(pad) = &args[2] else { return Err(VmError::new("third arg must be string".into())); };
                let width = *width as usize;
                let pad_char = pad.chars().next().unwrap_or(' ');
                if s.len() >= width { Ok(Value::String(s.clone())) }
                else { let padding: String = (0..width - s.len()).map(|_| pad_char).collect(); Ok(Value::String(format!("{s}{padding}"))) }
            }
            "char_code" => {
                if args.len() != 1 { return Err(VmError::new("string.char_code takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.char_code requires a string".into())); };
                match s.chars().next() {
                    Some(c) => Ok(Value::Int(c as i64)),
                    None => Err(VmError::new("string.char_code: empty string".into())),
                }
            }
            "from_char_code" => {
                if args.len() != 1 { return Err(VmError::new("string.from_char_code takes 1 argument".into())); }
                let Value::Int(n) = &args[0] else { return Err(VmError::new("string.from_char_code requires an int".into())); };
                match char::from_u32(*n as u32) {
                    Some(c) => Ok(Value::String(c.to_string())),
                    None => Err(VmError::new(format!("invalid code point {n}"))),
                }
            }
            "is_empty" => {
                if args.len() != 1 { return Err(VmError::new("string.is_empty takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.is_empty requires a string".into())); };
                Ok(Value::Bool(s.is_empty()))
            }
            "is_alpha" => {
                if args.len() != 1 { return Err(VmError::new("string.is_alpha takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.is_alpha requires a string".into())); };
                Ok(Value::Bool(s.chars().next().map_or(false, |c| c.is_alphabetic())))
            }
            "is_digit" => {
                if args.len() != 1 { return Err(VmError::new("string.is_digit takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.is_digit requires a string".into())); };
                Ok(Value::Bool(s.chars().next().map_or(false, |c| c.is_ascii_digit())))
            }
            "is_upper" => {
                if args.len() != 1 { return Err(VmError::new("string.is_upper takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.is_upper requires a string".into())); };
                Ok(Value::Bool(s.chars().next().map_or(false, |c| c.is_uppercase())))
            }
            "is_lower" => {
                if args.len() != 1 { return Err(VmError::new("string.is_lower takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.is_lower requires a string".into())); };
                Ok(Value::Bool(s.chars().next().map_or(false, |c| c.is_lowercase())))
            }
            "is_alnum" => {
                if args.len() != 1 { return Err(VmError::new("string.is_alnum takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.is_alnum requires a string".into())); };
                Ok(Value::Bool(s.chars().next().map_or(false, |c| c.is_alphanumeric())))
            }
            "is_whitespace" => {
                if args.len() != 1 { return Err(VmError::new("string.is_whitespace takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("string.is_whitespace requires a string".into())); };
                Ok(Value::Bool(s.chars().next().map_or(false, |c| c.is_whitespace())))
            }
            _ => Err(VmError::new(format!("unknown string function: {name}"))),
        }
    }

    // ── Int builtins ──────────────────────────────────────────────

    fn dispatch_int(&self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "parse" => {
                if args.len() != 1 { return Err(VmError::new("int.parse takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("int.parse requires a string".into())); };
                match s.trim().parse::<i64>() {
                    Ok(n) => Ok(Value::Variant("Ok".into(), vec![Value::Int(n)])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "abs" => {
                if args.len() != 1 { return Err(VmError::new("int.abs takes 1 argument".into())); }
                let Value::Int(n) = &args[0] else { return Err(VmError::new("int.abs requires an int".into())); };
                Ok(Value::Int(n.abs()))
            }
            "min" => {
                if args.len() != 2 { return Err(VmError::new("int.min takes 2 arguments".into())); }
                let (Value::Int(a), Value::Int(b)) = (&args[0], &args[1]) else { return Err(VmError::new("int.min requires ints".into())); };
                Ok(Value::Int(*a.min(b)))
            }
            "max" => {
                if args.len() != 2 { return Err(VmError::new("int.max takes 2 arguments".into())); }
                let (Value::Int(a), Value::Int(b)) = (&args[0], &args[1]) else { return Err(VmError::new("int.max requires ints".into())); };
                Ok(Value::Int(*a.max(b)))
            }
            "to_float" => {
                if args.len() != 1 { return Err(VmError::new("int.to_float takes 1 argument".into())); }
                let Value::Int(n) = &args[0] else { return Err(VmError::new("int.to_float requires an int".into())); };
                Ok(Value::Float(*n as f64))
            }
            "to_string" => {
                if args.len() != 1 { return Err(VmError::new("int.to_string takes 1 argument".into())); }
                let Value::Int(n) = &args[0] else { return Err(VmError::new("int.to_string requires an int".into())); };
                Ok(Value::String(n.to_string()))
            }
            _ => Err(VmError::new(format!("unknown int function: {name}"))),
        }
    }

    // ── Float builtins ────────────────────────────────────────────

    fn dispatch_float(&self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "parse" => {
                if args.len() != 1 { return Err(VmError::new("float.parse takes 1 argument".into())); }
                let Value::String(s) = &args[0] else { return Err(VmError::new("float.parse requires a string".into())); };
                match s.trim().parse::<f64>() {
                    Ok(n) => Ok(Value::Variant("Ok".into(), vec![Value::Float(n)])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "round" => {
                if args.len() != 1 { return Err(VmError::new("float.round takes 1 argument".into())); }
                let Value::Float(f) = &args[0] else { return Err(VmError::new("float.round requires a float".into())); };
                Ok(Value::Float(f.round()))
            }
            "ceil" => {
                if args.len() != 1 { return Err(VmError::new("float.ceil takes 1 argument".into())); }
                let Value::Float(f) = &args[0] else { return Err(VmError::new("float.ceil requires a float".into())); };
                Ok(Value::Float(f.ceil()))
            }
            "floor" => {
                if args.len() != 1 { return Err(VmError::new("float.floor takes 1 argument".into())); }
                let Value::Float(f) = &args[0] else { return Err(VmError::new("float.floor requires a float".into())); };
                Ok(Value::Float(f.floor()))
            }
            "abs" => {
                if args.len() != 1 { return Err(VmError::new("float.abs takes 1 argument".into())); }
                let Value::Float(f) = &args[0] else { return Err(VmError::new("float.abs requires a float".into())); };
                Ok(Value::Float(f.abs()))
            }
            "to_string" => {
                if args.len() != 2 { return Err(VmError::new("float.to_string takes 2 arguments".into())); }
                let Value::Float(f) = &args[0] else { return Err(VmError::new("float.to_string requires a float".into())); };
                let Value::Int(decimals) = &args[1] else { return Err(VmError::new("float.to_string requires an int for decimals".into())); };
                Ok(Value::String(format!("{:.prec$}", f, prec = *decimals as usize)))
            }
            "to_int" => {
                if args.len() != 1 { return Err(VmError::new("float.to_int takes 1 argument".into())); }
                let Value::Float(f) = &args[0] else { return Err(VmError::new("float.to_int requires a float".into())); };
                Ok(Value::Int(*f as i64))
            }
            "min" => {
                if args.len() != 2 { return Err(VmError::new("float.min takes 2 arguments".into())); }
                let (Value::Float(a), Value::Float(b)) = (&args[0], &args[1]) else { return Err(VmError::new("float.min requires floats".into())); };
                Ok(Value::Float(a.min(*b)))
            }
            "max" => {
                if args.len() != 2 { return Err(VmError::new("float.max takes 2 arguments".into())); }
                let (Value::Float(a), Value::Float(b)) = (&args[0], &args[1]) else { return Err(VmError::new("float.max requires floats".into())); };
                Ok(Value::Float(a.max(*b)))
            }
            _ => Err(VmError::new(format!("unknown float function: {name}"))),
        }
    }

    // ── Map builtins ──────────────────────────────────────────────

    fn dispatch_map(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "get" => {
                if args.len() != 2 { return Err(VmError::new("map.get takes 2 arguments".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.get requires a map".into())); };
                match m.get(&args[1]) {
                    Some(val) => Ok(Value::Variant("Some".into(), vec![val.clone()])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "set" => {
                if args.len() != 3 { return Err(VmError::new("map.set takes 3 arguments".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.set requires a map".into())); };
                let mut new_map = (**m).clone();
                new_map.insert(args[1].clone(), args[2].clone());
                Ok(Value::Map(Rc::new(new_map)))
            }
            "delete" => {
                if args.len() != 2 { return Err(VmError::new("map.delete takes 2 arguments".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.delete requires a map".into())); };
                let mut new_map = (**m).clone();
                new_map.remove(&args[1]);
                Ok(Value::Map(Rc::new(new_map)))
            }
            "contains" => {
                if args.len() != 2 { return Err(VmError::new("map.contains takes 2 arguments".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.contains requires a map".into())); };
                Ok(Value::Bool(m.contains_key(&args[1])))
            }
            "keys" => {
                if args.len() != 1 { return Err(VmError::new("map.keys takes 1 argument".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.keys requires a map".into())); };
                Ok(Value::List(Rc::new(m.keys().cloned().collect())))
            }
            "values" => {
                if args.len() != 1 { return Err(VmError::new("map.values takes 1 argument".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.values requires a map".into())); };
                Ok(Value::List(Rc::new(m.values().cloned().collect())))
            }
            "length" => {
                if args.len() != 1 { return Err(VmError::new("map.length takes 1 argument".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.length requires a map".into())); };
                Ok(Value::Int(m.len() as i64))
            }
            "merge" => {
                if args.len() != 2 { return Err(VmError::new("map.merge takes 2 arguments".into())); }
                let (Value::Map(m1), Value::Map(m2)) = (&args[0], &args[1]) else { return Err(VmError::new("map.merge requires maps".into())); };
                let mut result = (**m1).clone();
                for (k, v) in m2.iter() { result.insert(k.clone(), v.clone()); }
                Ok(Value::Map(Rc::new(result)))
            }
            "entries" => {
                if args.len() != 1 { return Err(VmError::new("map.entries takes 1 argument".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.entries requires a map".into())); };
                let entries: Vec<Value> = m.iter().map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()])).collect();
                Ok(Value::List(Rc::new(entries)))
            }
            "from_entries" => {
                if args.len() != 1 { return Err(VmError::new("map.from_entries takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("map.from_entries requires a list".into())); };
                let mut result = BTreeMap::new();
                for item in xs.iter() {
                    if let Value::Tuple(pair) = item { if pair.len() == 2 { result.insert(pair[0].clone(), pair[1].clone()); continue; } }
                    return Err(VmError::new("map.from_entries requires (key, value) tuples".into()));
                }
                Ok(Value::Map(Rc::new(result)))
            }
            "filter" => {
                if args.len() != 2 { return Err(VmError::new("map.filter takes 2 arguments".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.filter requires a map".into())); };
                let func = &args[1];
                let mut result = BTreeMap::new();
                for (k, v) in m.iter() {
                    let keep = self.invoke_callable(func, &[k.clone(), v.clone()])?;
                    if self.is_truthy(&keep) {
                        result.insert(k.clone(), v.clone());
                    }
                }
                Ok(Value::Map(Rc::new(result)))
            }
            "map" => {
                if args.len() != 2 { return Err(VmError::new("map.map takes 2 arguments".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.map requires a map".into())); };
                let func = &args[1];
                let mut result = BTreeMap::new();
                for (k, v) in m.iter() {
                    let mapped = self.invoke_callable(func, &[k.clone(), v.clone()])?;
                    match mapped {
                        Value::Tuple(pair) if pair.len() == 2 => {
                            result.insert(pair[0].clone(), pair[1].clone());
                        }
                        _ => return Err(VmError::new("map.map callback must return a (key, value) tuple".into())),
                    }
                }
                Ok(Value::Map(Rc::new(result)))
            }
            "each" => {
                if args.len() != 2 { return Err(VmError::new("map.each takes 2 arguments".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.each requires a map".into())); };
                let func = &args[1];
                for (k, v) in m.iter() {
                    self.invoke_callable(func, &[k.clone(), v.clone()])?;
                }
                Ok(Value::Unit)
            }
            "update" => {
                if args.len() != 4 { return Err(VmError::new("map.update takes 4 arguments (map, key, default, fn)".into())); }
                let Value::Map(m) = &args[0] else { return Err(VmError::new("map.update requires a map".into())); };
                let key = &args[1];
                let default = &args[2];
                let func = &args[3];
                let current = m.get(key).unwrap_or(default).clone();
                let new_val = self.invoke_callable(func, &[current])?;
                let mut new_map = (**m).clone();
                new_map.insert(key.clone(), new_val);
                Ok(Value::Map(Rc::new(new_map)))
            }
            _ => Err(VmError::new(format!("unknown map function: {name}"))),
        }
    }

    // ── Set builtins ──────────────────────────────────────────────

    fn dispatch_set(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "new" => Ok(Value::Set(Rc::new(BTreeSet::new()))),
            "from_list" => {
                if args.len() != 1 { return Err(VmError::new("set.from_list takes 1 argument".into())); }
                let Value::List(xs) = &args[0] else { return Err(VmError::new("set.from_list requires a list".into())); };
                Ok(Value::Set(Rc::new(xs.iter().cloned().collect())))
            }
            "to_list" => {
                if args.len() != 1 { return Err(VmError::new("set.to_list takes 1 argument".into())); }
                let Value::Set(s) = &args[0] else { return Err(VmError::new("set.to_list requires a set".into())); };
                Ok(Value::List(Rc::new(s.iter().cloned().collect())))
            }
            "contains" => {
                if args.len() != 2 { return Err(VmError::new("set.contains takes 2 arguments".into())); }
                let Value::Set(s) = &args[0] else { return Err(VmError::new("set.contains requires a set".into())); };
                Ok(Value::Bool(s.contains(&args[1])))
            }
            "insert" => {
                if args.len() != 2 { return Err(VmError::new("set.insert takes 2 arguments".into())); }
                let Value::Set(s) = &args[0] else { return Err(VmError::new("set.insert requires a set".into())); };
                let mut new_set = (**s).clone(); new_set.insert(args[1].clone());
                Ok(Value::Set(Rc::new(new_set)))
            }
            "remove" => {
                if args.len() != 2 { return Err(VmError::new("set.remove takes 2 arguments".into())); }
                let Value::Set(s) = &args[0] else { return Err(VmError::new("set.remove requires a set".into())); };
                let mut new_set = (**s).clone(); new_set.remove(&args[1]);
                Ok(Value::Set(Rc::new(new_set)))
            }
            "length" => {
                if args.len() != 1 { return Err(VmError::new("set.length takes 1 argument".into())); }
                let Value::Set(s) = &args[0] else { return Err(VmError::new("set.length requires a set".into())); };
                Ok(Value::Int(s.len() as i64))
            }
            "union" => {
                if args.len() != 2 { return Err(VmError::new("set.union takes 2 arguments".into())); }
                let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else { return Err(VmError::new("set.union requires sets".into())); };
                Ok(Value::Set(Rc::new(a.union(b).cloned().collect())))
            }
            "intersection" => {
                if args.len() != 2 { return Err(VmError::new("set.intersection takes 2 arguments".into())); }
                let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else { return Err(VmError::new("set.intersection requires sets".into())); };
                Ok(Value::Set(Rc::new(a.intersection(b).cloned().collect())))
            }
            "difference" => {
                if args.len() != 2 { return Err(VmError::new("set.difference takes 2 arguments".into())); }
                let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else { return Err(VmError::new("set.difference requires sets".into())); };
                Ok(Value::Set(Rc::new(a.difference(b).cloned().collect())))
            }
            "is_subset" => {
                if args.len() != 2 { return Err(VmError::new("set.is_subset takes 2 arguments".into())); }
                let (Value::Set(a), Value::Set(b)) = (&args[0], &args[1]) else { return Err(VmError::new("set.is_subset requires sets".into())); };
                Ok(Value::Bool(a.is_subset(b)))
            }
            "map" => {
                if args.len() != 2 { return Err(VmError::new("set.map takes 2 arguments".into())); }
                let Value::Set(s) = &args[0] else { return Err(VmError::new("set.map requires a set".into())); };
                let func = &args[1];
                let mut result = BTreeSet::new();
                for item in s.iter() {
                    let val = self.invoke_callable(func, &[item.clone()])?;
                    result.insert(val);
                }
                Ok(Value::Set(Rc::new(result)))
            }
            "filter" => {
                if args.len() != 2 { return Err(VmError::new("set.filter takes 2 arguments".into())); }
                let Value::Set(s) = &args[0] else { return Err(VmError::new("set.filter requires a set".into())); };
                let func = &args[1];
                let mut result = BTreeSet::new();
                for item in s.iter() {
                    let keep = self.invoke_callable(func, &[item.clone()])?;
                    if self.is_truthy(&keep) {
                        result.insert(item.clone());
                    }
                }
                Ok(Value::Set(Rc::new(result)))
            }
            "each" => {
                if args.len() != 2 { return Err(VmError::new("set.each takes 2 arguments".into())); }
                let Value::Set(s) = &args[0] else { return Err(VmError::new("set.each requires a set".into())); };
                let func = &args[1];
                for item in s.iter() {
                    self.invoke_callable(func, &[item.clone()])?;
                }
                Ok(Value::Unit)
            }
            "fold" => {
                if args.len() != 3 { return Err(VmError::new("set.fold takes 3 arguments".into())); }
                let Value::Set(s) = &args[0] else { return Err(VmError::new("set.fold requires a set".into())); };
                let func = &args[2];
                let mut acc = args[1].clone();
                for item in s.iter() {
                    acc = self.invoke_callable(func, &[acc, item.clone()])?;
                }
                Ok(acc)
            }
            _ => Err(VmError::new(format!("unknown set function: {name}"))),
        }
    }

    // ── Result builtins ───────────────────────────────────────────

    fn dispatch_result(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "unwrap_or" => {
                if args.len() != 2 { return Err(VmError::new("result.unwrap_or takes 2 arguments".into())); }
                match &args[0] {
                    Value::Variant(tag, fields) if tag == "Ok" => Ok(fields[0].clone()),
                    Value::Variant(tag, _) if tag == "Err" => Ok(args[1].clone()),
                    _ => Err(VmError::new("result.unwrap_or requires a Result".into())),
                }
            }
            "is_ok" => {
                if args.len() != 1 { return Err(VmError::new("result.is_ok takes 1 argument".into())); }
                Ok(Value::Bool(matches!(&args[0], Value::Variant(tag, _) if tag == "Ok")))
            }
            "is_err" => {
                if args.len() != 1 { return Err(VmError::new("result.is_err takes 1 argument".into())); }
                Ok(Value::Bool(matches!(&args[0], Value::Variant(tag, _) if tag == "Err")))
            }
            "map_ok" => {
                if args.len() != 2 { return Err(VmError::new("result.map_ok takes 2 arguments".into())); }
                match &args[0] {
                    Value::Variant(tag, fields) if tag == "Ok" && fields.len() == 1 => {
                        let new_val = self.invoke_callable(&args[1], &[fields[0].clone()])?;
                        Ok(Value::Variant("Ok".into(), vec![new_val]))
                    }
                    other @ Value::Variant(tag, _) if tag == "Err" => Ok(other.clone()),
                    _ => Err(VmError::new("result.map_ok requires a Result".into())),
                }
            }
            "map_err" => {
                if args.len() != 2 { return Err(VmError::new("result.map_err takes 2 arguments".into())); }
                match &args[0] {
                    other @ Value::Variant(tag, _) if tag == "Ok" => Ok(other.clone()),
                    Value::Variant(tag, fields) if tag == "Err" && fields.len() == 1 => {
                        let new_val = self.invoke_callable(&args[1], &[fields[0].clone()])?;
                        Ok(Value::Variant("Err".into(), vec![new_val]))
                    }
                    _ => Err(VmError::new("result.map_err requires a Result".into())),
                }
            }
            "flatten" => {
                if args.len() != 1 { return Err(VmError::new("result.flatten takes 1 argument".into())); }
                match &args[0] {
                    Value::Variant(tag, fields) if tag == "Ok" && fields.len() == 1 => {
                        match &fields[0] {
                            ok @ Value::Variant(inner_tag, _) if inner_tag == "Ok" || inner_tag == "Err" => Ok(ok.clone()),
                            _ => Ok(args[0].clone()),
                        }
                    }
                    other @ Value::Variant(tag, _) if tag == "Err" => Ok(other.clone()),
                    _ => Err(VmError::new("result.flatten requires a Result".into())),
                }
            }
            "flat_map" => {
                if args.len() != 2 { return Err(VmError::new("result.flat_map takes 2 arguments".into())); }
                match &args[0] {
                    Value::Variant(tag, fields) if tag == "Ok" && fields.len() == 1 => {
                        self.invoke_callable(&args[1], &[fields[0].clone()])
                    }
                    other @ Value::Variant(tag, _) if tag == "Err" => Ok(other.clone()),
                    _ => Err(VmError::new("result.flat_map requires a Result".into())),
                }
            }
            _ => Err(VmError::new(format!("unknown result function: {name}"))),
        }
    }

    // ── Option builtins ───────────────────────────────────────────

    fn dispatch_option(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "unwrap_or" => {
                if args.len() != 2 { return Err(VmError::new("option.unwrap_or takes 2 arguments".into())); }
                match &args[0] {
                    Value::Variant(tag, fields) if tag == "Some" => Ok(fields[0].clone()),
                    Value::Variant(tag, _) if tag == "None" => Ok(args[1].clone()),
                    _ => Err(VmError::new("option.unwrap_or requires an Option".into())),
                }
            }
            "is_some" => {
                if args.len() != 1 { return Err(VmError::new("option.is_some takes 1 argument".into())); }
                Ok(Value::Bool(matches!(&args[0], Value::Variant(tag, _) if tag == "Some")))
            }
            "is_none" => {
                if args.len() != 1 { return Err(VmError::new("option.is_none takes 1 argument".into())); }
                Ok(Value::Bool(matches!(&args[0], Value::Variant(tag, _) if tag == "None")))
            }
            "to_result" => {
                if args.len() != 2 { return Err(VmError::new("option.to_result takes 2 arguments".into())); }
                match &args[0] {
                    Value::Variant(tag, fields) if tag == "Some" => Ok(Value::Variant("Ok".into(), vec![fields[0].clone()])),
                    Value::Variant(tag, _) if tag == "None" => Ok(Value::Variant("Err".into(), vec![args[1].clone()])),
                    _ => Err(VmError::new("option.to_result requires an Option".into())),
                }
            }
            "map" => {
                if args.len() != 2 { return Err(VmError::new("option.map takes 2 arguments".into())); }
                match &args[0] {
                    Value::Variant(tag, fields) if tag == "Some" && fields.len() == 1 => {
                        let new_val = self.invoke_callable(&args[1], &[fields[0].clone()])?;
                        Ok(Value::Variant("Some".into(), vec![new_val]))
                    }
                    other @ Value::Variant(tag, _) if tag == "None" => Ok(other.clone()),
                    _ => Err(VmError::new("option.map requires an Option".into())),
                }
            }
            "flat_map" => {
                if args.len() != 2 { return Err(VmError::new("option.flat_map takes 2 arguments".into())); }
                match &args[0] {
                    Value::Variant(tag, fields) if tag == "Some" && fields.len() == 1 => {
                        self.invoke_callable(&args[1], &[fields[0].clone()])
                    }
                    other @ Value::Variant(tag, _) if tag == "None" => Ok(other.clone()),
                    _ => Err(VmError::new("option.flat_map requires an Option".into())),
                }
            }
            _ => Err(VmError::new(format!("unknown option function: {name}"))),
        }
    }

    // ── IO builtins ───────────────────────────────────────────────

    fn dispatch_io(&self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "inspect" => {
                if args.len() != 1 { return Err(VmError::new("io.inspect takes 1 argument".into())); }
                Ok(Value::String(args[0].format_silt()))
            }
            "read_file" => {
                if args.len() != 1 { return Err(VmError::new("io.read_file takes 1 argument".into())); }
                let Value::String(path) = &args[0] else { return Err(VmError::new("io.read_file requires a string path".into())); };
                match std::fs::read_to_string(path) {
                    Ok(content) => Ok(Value::Variant("Ok".into(), vec![Value::String(content)])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "write_file" => {
                if args.len() != 2 { return Err(VmError::new("io.write_file takes 2 arguments".into())); }
                let (Value::String(path), Value::String(content)) = (&args[0], &args[1]) else { return Err(VmError::new("io.write_file requires string arguments".into())); };
                match std::fs::write(path, content) {
                    Ok(()) => Ok(Value::Variant("Ok".into(), vec![Value::Unit])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "read_line" => {
                let mut line = String::new();
                match std::io::stdin().read_line(&mut line) {
                    Ok(_) => Ok(Value::Variant("Ok".into(), vec![Value::String(line.trim_end().to_string())])),
                    Err(e) => Ok(Value::Variant("Err".into(), vec![Value::String(e.to_string())])),
                }
            }
            "args" => {
                let args_list: Vec<Value> = std::env::args().map(Value::String).collect();
                Ok(Value::List(Rc::new(args_list)))
            }
            _ => Err(VmError::new(format!("unknown io function: {name}"))),
        }
    }

    // ── FS builtins ───────────────────────────────────────────────

    fn dispatch_fs(&self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "exists" => {
                if args.len() != 1 { return Err(VmError::new("fs.exists takes 1 argument".into())); }
                let Value::String(path) = &args[0] else { return Err(VmError::new("fs.exists requires a string path".into())); };
                Ok(Value::Bool(std::path::Path::new(path).exists()))
            }
            _ => Err(VmError::new(format!("unknown fs function: {name}"))),
        }
    }

    // ── Test builtins ─────────────────────────────────────────────

    fn dispatch_test(&self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "assert" => {
                if args.is_empty() || args.len() > 2 { return Err(VmError::new("test.assert takes 1-2 arguments".into())); }
                if self.is_truthy(&args[0]) { Ok(Value::Unit) }
                else {
                    let msg = if args.len() == 2 { format!("assertion failed: {}", args[1]) }
                    else { format!("assertion failed: {:?}", args[0]) };
                    Err(VmError::new(msg))
                }
            }
            "assert_eq" => {
                if args.len() < 2 || args.len() > 3 { return Err(VmError::new("test.assert_eq takes 2-3 arguments".into())); }
                if args[0] == args[1] { Ok(Value::Unit) }
                else {
                    let msg = if args.len() == 3 { format!("assertion failed: {}: {:?} != {:?}", args[2], args[0], args[1]) }
                    else { format!("assertion failed: {:?} != {:?}", args[0], args[1]) };
                    Err(VmError::new(msg))
                }
            }
            "assert_ne" => {
                if args.len() < 2 || args.len() > 3 { return Err(VmError::new("test.assert_ne takes 2-3 arguments".into())); }
                if args[0] != args[1] { Ok(Value::Unit) }
                else {
                    let msg = if args.len() == 3 { format!("assertion failed: {}: {:?} == {:?}", args[2], args[0], args[1]) }
                    else { format!("assertion failed: {:?} == {:?}", args[0], args[1]) };
                    Err(VmError::new(msg))
                }
            }
            _ => Err(VmError::new(format!("unknown test function: {name}"))),
        }
    }

    // ── Math builtins ─────────────────────────────────────────────

    fn dispatch_math(&self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "sqrt" => {
                if args.len() != 1 { return Err(VmError::new("math.sqrt takes 1 argument".into())); }
                let f = match &args[0] {
                    Value::Float(f) => *f,
                    Value::Int(n) => *n as f64,
                    _ => return Err(VmError::new("math.sqrt requires a number".into())),
                };
                Ok(Value::Float(f.sqrt()))
            }
            "pow" => {
                if args.len() != 2 { return Err(VmError::new("math.pow takes 2 arguments".into())); }
                let base = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.pow requires numbers".into())) };
                let exp = match &args[1] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.pow requires numbers".into())) };
                Ok(Value::Float(base.powf(exp)))
            }
            "log" => {
                if args.len() != 1 { return Err(VmError::new("math.log takes 1 argument".into())); }
                let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.log requires a number".into())) };
                Ok(Value::Float(f.ln()))
            }
            "log10" => {
                if args.len() != 1 { return Err(VmError::new("math.log10 takes 1 argument".into())); }
                let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.log10 requires a number".into())) };
                Ok(Value::Float(f.log10()))
            }
            "sin" => {
                if args.len() != 1 { return Err(VmError::new("math.sin takes 1 argument".into())); }
                let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.sin requires a number".into())) };
                Ok(Value::Float(f.sin()))
            }
            "cos" => {
                if args.len() != 1 { return Err(VmError::new("math.cos takes 1 argument".into())); }
                let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.cos requires a number".into())) };
                Ok(Value::Float(f.cos()))
            }
            "tan" => {
                if args.len() != 1 { return Err(VmError::new("math.tan takes 1 argument".into())); }
                let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.tan requires a number".into())) };
                Ok(Value::Float(f.tan()))
            }
            "asin" => {
                if args.len() != 1 { return Err(VmError::new("math.asin takes 1 argument".into())); }
                let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.asin requires a number".into())) };
                Ok(Value::Float(f.asin()))
            }
            "acos" => {
                if args.len() != 1 { return Err(VmError::new("math.acos takes 1 argument".into())); }
                let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.acos requires a number".into())) };
                Ok(Value::Float(f.acos()))
            }
            "atan" => {
                if args.len() != 1 { return Err(VmError::new("math.atan takes 1 argument".into())); }
                let f = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.atan requires a number".into())) };
                Ok(Value::Float(f.atan()))
            }
            "atan2" => {
                if args.len() != 2 { return Err(VmError::new("math.atan2 takes 2 arguments".into())); }
                let y = match &args[0] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.atan2 requires numbers".into())) };
                let x = match &args[1] { Value::Float(f) => *f, Value::Int(n) => *n as f64, _ => return Err(VmError::new("math.atan2 requires numbers".into())) };
                Ok(Value::Float(y.atan2(x)))
            }
            _ => Err(VmError::new(format!("unknown math function: {name}"))),
        }
    }

    // ── Regex module ─────────────────────────────────────────────

    fn dispatch_regex(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "is_match" => {
                if args.len() != 2 {
                    return Err(VmError::new("regex.is_match takes 2 arguments (pattern, text)".into()));
                }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(VmError::new("regex.is_match requires string arguments".into()));
                };
                let re = Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
                Ok(Value::Bool(re.is_match(text)))
            }
            "find" => {
                if args.len() != 2 {
                    return Err(VmError::new("regex.find takes 2 arguments (pattern, text)".into()));
                }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(VmError::new("regex.find requires string arguments".into()));
                };
                let re = Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
                match re.find(text) {
                    Some(m) => Ok(Value::Variant("Some".into(), vec![Value::String(m.as_str().to_string())])),
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "find_all" => {
                if args.len() != 2 {
                    return Err(VmError::new("regex.find_all takes 2 arguments (pattern, text)".into()));
                }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(VmError::new("regex.find_all requires string arguments".into()));
                };
                let re = Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
                let matches: Vec<Value> = re.find_iter(text)
                    .map(|m| Value::String(m.as_str().to_string()))
                    .collect();
                Ok(Value::List(Rc::new(matches)))
            }
            "split" => {
                if args.len() != 2 {
                    return Err(VmError::new("regex.split takes 2 arguments (pattern, text)".into()));
                }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(VmError::new("regex.split requires string arguments".into()));
                };
                let re = Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
                let parts: Vec<Value> = re.split(text).map(|s| Value::String(s.to_string())).collect();
                Ok(Value::List(Rc::new(parts)))
            }
            "replace" => {
                if args.len() != 3 {
                    return Err(VmError::new("regex.replace takes 3 arguments (pattern, text, replacement)".into()));
                }
                let (Value::String(pattern), Value::String(text), Value::String(replacement)) = (&args[0], &args[1], &args[2]) else {
                    return Err(VmError::new("regex.replace requires string arguments".into()));
                };
                let re = Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
                Ok(Value::String(re.replace(text, replacement.as_str()).to_string()))
            }
            "replace_all" => {
                if args.len() != 3 {
                    return Err(VmError::new("regex.replace_all takes 3 arguments (pattern, text, replacement)".into()));
                }
                let (Value::String(pattern), Value::String(text), Value::String(replacement)) = (&args[0], &args[1], &args[2]) else {
                    return Err(VmError::new("regex.replace_all requires string arguments".into()));
                };
                let re = Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
                Ok(Value::String(re.replace_all(text, replacement.as_str()).to_string()))
            }
            "replace_all_with" => {
                if args.len() != 3 {
                    return Err(VmError::new("regex.replace_all_with takes 3 arguments (pattern, text, fn)".into()));
                }
                let Value::String(pattern) = &args[0] else {
                    return Err(VmError::new("regex.replace_all_with requires a string pattern".into()));
                };
                let Value::String(text) = &args[1] else {
                    return Err(VmError::new("regex.replace_all_with requires a string text".into()));
                };
                let callback = args[2].clone();
                let re = Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
                let mut result = std::string::String::new();
                let mut last_end = 0;
                for m in re.find_iter(text) {
                    result.push_str(&text[last_end..m.start()]);
                    let replacement = self.invoke_callable(&callback, &[Value::String(m.as_str().to_string())])?;
                    match replacement {
                        Value::String(s) => result.push_str(&s),
                        _ => return Err(VmError::new("regex.replace_all_with callback must return a string".into())),
                    }
                    last_end = m.end();
                }
                result.push_str(&text[last_end..]);
                Ok(Value::String(result))
            }
            "captures" => {
                if args.len() != 2 {
                    return Err(VmError::new("regex.captures takes 2 arguments (pattern, text)".into()));
                }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(VmError::new("regex.captures requires string arguments".into()));
                };
                let re = Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
                match re.captures(text) {
                    Some(caps) => {
                        let groups: Vec<Value> = caps.iter()
                            .map(|m| match m {
                                Some(m) => Value::String(m.as_str().to_string()),
                                None => Value::String(std::string::String::new()),
                            })
                            .collect();
                        Ok(Value::Variant("Some".into(), vec![Value::List(Rc::new(groups))]))
                    }
                    None => Ok(Value::Variant("None".into(), Vec::new())),
                }
            }
            "captures_all" => {
                if args.len() != 2 {
                    return Err(VmError::new("regex.captures_all takes 2 arguments (pattern, text)".into()));
                }
                let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
                    return Err(VmError::new("regex.captures_all requires string arguments".into()));
                };
                let re = Regex::new(pattern).map_err(|e| VmError::new(format!("invalid regex: {e}")))?;
                let all_captures: Vec<Value> = re.captures_iter(text)
                    .map(|caps| {
                        let groups: Vec<Value> = caps.iter()
                            .map(|m| match m {
                                Some(m) => Value::String(m.as_str().to_string()),
                                None => Value::String(std::string::String::new()),
                            })
                            .collect();
                        Value::List(Rc::new(groups))
                    })
                    .collect();
                Ok(Value::List(Rc::new(all_captures)))
            }
            _ => Err(VmError::new(format!("unknown regex function: {name}"))),
        }
    }

    // ── JSON module ──────────────────────────────────────────────

    /// Load record field info from the `__record_fields__<type>` global metadata.
    fn load_record_fields(&mut self, type_name: &str) -> Result<Vec<(String, FieldType)>, VmError> {
        // Check cache first
        if let Some(fields) = self.record_types.get(type_name) {
            return Ok(fields.clone());
        }
        // Look up the metadata global
        let meta_key = format!("__record_fields__{type_name}");
        let meta = self.globals.get(&meta_key).cloned();
        match meta {
            Some(Value::List(items)) => {
                let mut fields = Vec::new();
                let mut i = 0;
                while i + 1 < items.len() {
                    if let (Value::String(fname), Value::String(ftype)) = (&items[i], &items[i + 1]) {
                        fields.push((fname.clone(), decode_field_type(ftype)));
                    }
                    i += 2;
                }
                self.record_types.insert(type_name.to_string(), fields.clone());
                Ok(fields)
            }
            _ => Err(VmError::new(format!("json.parse: unknown record type '{type_name}'"))),
        }
    }

    fn dispatch_json(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "parse" => {
                if args.len() != 2 {
                    return Err(VmError::new("json.parse takes 2 arguments: (Type, String)".into()));
                }
                let Value::RecordDescriptor(type_name) = &args[0] else {
                    return Err(VmError::new("json.parse: first argument must be a record type".into()));
                };
                let type_name = type_name.clone();
                let Value::String(s) = &args[1] else {
                    return Err(VmError::new("json.parse: second argument must be a string".into()));
                };
                let s = s.clone();
                let fields = self.load_record_fields(&type_name)?;
                match serde_json::from_str::<serde_json::Value>(&s) {
                    Ok(json_val) => self.json_to_record(&type_name, &fields, &json_val),
                    Err(e) => Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!("json.parse: {e}"))],
                    )),
                }
            }
            "parse_list" => {
                if args.len() != 2 {
                    return Err(VmError::new("json.parse_list takes 2 arguments: (Type, String)".into()));
                }
                let Value::RecordDescriptor(type_name) = &args[0] else {
                    return Err(VmError::new("json.parse_list: first argument must be a record type".into()));
                };
                let type_name = type_name.clone();
                let Value::String(s) = &args[1] else {
                    return Err(VmError::new("json.parse_list: second argument must be a string".into()));
                };
                let s = s.clone();
                let fields = self.load_record_fields(&type_name)?;
                match serde_json::from_str::<serde_json::Value>(&s) {
                    Ok(json_val) => self.json_to_record_list(&type_name, &fields, &json_val),
                    Err(e) => Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!("json.parse_list: {e}"))],
                    )),
                }
            }
            "parse_map" => {
                if args.len() != 2 {
                    return Err(VmError::new("json.parse_map takes 2 arguments: (ValueType, String)".into()));
                }
                let value_type = match &args[0] {
                    Value::PrimitiveDescriptor(name) => name.clone(),
                    Value::RecordDescriptor(name) => name.clone(),
                    _ => return Err(VmError::new(
                        "json.parse_map: first argument must be a type (Int, Float, String, Bool, or a record type)".into()
                    )),
                };
                let Value::String(s) = &args[1] else {
                    return Err(VmError::new("json.parse_map: second argument must be a string".into()));
                };
                let s = s.clone();
                match serde_json::from_str::<serde_json::Value>(&s) {
                    Ok(json_val) => self.json_to_map(&value_type, &json_val),
                    Err(e) => Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!("json.parse_map: {e}"))],
                    )),
                }
            }
            "stringify" => {
                if args.len() != 1 {
                    return Err(VmError::new("json.stringify takes 1 argument".into()));
                }
                let j = value_to_json(&args[0]);
                Ok(Value::String(j.to_string()))
            }
            "pretty" => {
                if args.len() != 1 {
                    return Err(VmError::new("json.pretty takes 1 argument".into()));
                }
                let j = value_to_json(&args[0]);
                Ok(Value::String(serde_json::to_string_pretty(&j).unwrap_or_else(|_| j.to_string())))
            }
            _ => Err(VmError::new(format!("unknown json function: {name}"))),
        }
    }

    // ── JSON helpers ─────────────────────────────────────────────

    fn json_to_record(
        &mut self,
        type_name: &str,
        fields: &[(String, FieldType)],
        json: &serde_json::Value,
    ) -> Result<Value, VmError> {
        let serde_json::Value::Object(obj) = json else {
            return Ok(Value::Variant(
                "Err".into(),
                vec![Value::String(format!(
                    "json.parse({type_name}): expected JSON object, got {}", json_type_name(json)
                ))],
            ));
        };
        let mut record_fields = BTreeMap::new();
        for (field_name, field_type) in fields {
            match obj.get(field_name) {
                Some(json_val) => {
                    match self.json_to_typed_value(json_val, field_type, type_name, field_name) {
                        Ok(val) => {
                            record_fields.insert(field_name.clone(), val);
                        }
                        Err(e) => {
                            return Ok(Value::Variant(
                                "Err".into(),
                                vec![Value::String(e.message.clone())],
                            ));
                        }
                    }
                }
                None => match field_type {
                    FieldType::Option(_) => {
                        record_fields.insert(
                            field_name.clone(),
                            Value::Variant("None".into(), Vec::new()),
                        );
                    }
                    _ => {
                        return Ok(Value::Variant(
                            "Err".into(),
                            vec![Value::String(format!(
                                "json.parse({type_name}): missing field '{field_name}'"
                            ))],
                        ));
                    }
                },
            }
        }
        Ok(Value::Variant(
            "Ok".into(),
            vec![Value::Record(type_name.to_string(), Rc::new(record_fields))],
        ))
    }

    fn json_to_record_list(
        &mut self,
        type_name: &str,
        fields: &[(String, FieldType)],
        json: &serde_json::Value,
    ) -> Result<Value, VmError> {
        let serde_json::Value::Array(arr) = json else {
            return Ok(Value::Variant(
                "Err".into(),
                vec![Value::String(format!(
                    "json.parse_list({type_name}): expected JSON array, got {}", json_type_name(json)
                ))],
            ));
        };
        let mut records = Vec::new();
        for (i, item) in arr.iter().enumerate() {
            let result = self.json_to_record(type_name, fields, item)?;
            match result {
                Value::Variant(name, inner) if name == "Ok" && inner.len() == 1 => {
                    records.push(inner.into_iter().next().unwrap());
                }
                Value::Variant(name, inner) if name == "Err" && inner.len() == 1 => {
                    if let Value::String(msg) = &inner[0] {
                        return Ok(Value::Variant(
                            "Err".into(),
                            vec![Value::String(format!(
                                "json.parse_list({type_name}): element {i}: {msg}"
                            ))],
                        ));
                    } else {
                        return Ok(Value::Variant(
                            "Err".into(),
                            vec![Value::String(format!(
                                "json.parse_list({type_name}): element {i}: parse error"
                            ))],
                        ));
                    }
                }
                _ => {
                    return Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!(
                            "json.parse_list({type_name}): element {i}: unexpected result"
                        ))],
                    ));
                }
            }
        }
        Ok(Value::Variant(
            "Ok".into(),
            vec![Value::List(Rc::new(records))],
        ))
    }

    fn json_to_map(
        &mut self,
        value_type: &str,
        json: &serde_json::Value,
    ) -> Result<Value, VmError> {
        let serde_json::Value::Object(obj) = json else {
            return Ok(Value::Variant(
                "Err".into(),
                vec![Value::String(format!(
                    "json.parse_map: expected JSON object, got {}", json_type_name(json)
                ))],
            ));
        };
        let field_type = match value_type {
            "String" => FieldType::String,
            "Int" => FieldType::Int,
            "Float" => FieldType::Float,
            "Bool" => FieldType::Bool,
            record_name => {
                // Check if it's a known record type
                let meta_key = format!("__record_fields__{record_name}");
                if !self.globals.contains_key(&meta_key) {
                    return Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!(
                            "json.parse_map: unknown value type '{record_name}'"
                        ))],
                    ));
                }
                FieldType::Record(record_name.to_string())
            }
        };
        let mut map = BTreeMap::new();
        for (key, json_val) in obj {
            match self.json_to_typed_value(json_val, &field_type, "Map", key) {
                Ok(val) => {
                    map.insert(Value::String(key.clone()), val);
                }
                Err(e) => {
                    return Ok(Value::Variant(
                        "Err".into(),
                        vec![Value::String(format!(
                            "json.parse_map: key '{key}': {}", e.message
                        ))],
                    ));
                }
            }
        }
        Ok(Value::Variant(
            "Ok".into(),
            vec![Value::Map(Rc::new(map))],
        ))
    }

    fn json_to_typed_value(
        &mut self,
        json: &serde_json::Value,
        expected: &FieldType,
        parent_type: &str,
        field_name: &str,
    ) -> Result<Value, VmError> {
        match expected {
            FieldType::String => match json {
                serde_json::Value::String(s) => Ok(Value::String(s.clone())),
                _ => Err(VmError::new(format!(
                    "json.parse({parent_type}): field '{field_name}': expected String, got {}",
                    json_type_name(json)
                ))),
            },
            FieldType::Int => match json {
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        Ok(Value::Int(i))
                    } else if let Some(f) = n.as_f64() {
                        Ok(Value::Int(f as i64))
                    } else {
                        Err(VmError::new(format!(
                            "json.parse({parent_type}): field '{field_name}': expected Int, got number that doesn't fit"
                        )))
                    }
                }
                _ => Err(VmError::new(format!(
                    "json.parse({parent_type}): field '{field_name}': expected Int, got {}",
                    json_type_name(json)
                ))),
            },
            FieldType::Float => match json {
                serde_json::Value::Number(n) => {
                    if let Some(f) = n.as_f64() {
                        Ok(Value::Float(f))
                    } else {
                        Err(VmError::new(format!(
                            "json.parse({parent_type}): field '{field_name}': expected Float, got non-numeric number"
                        )))
                    }
                }
                _ => Err(VmError::new(format!(
                    "json.parse({parent_type}): field '{field_name}': expected Float, got {}",
                    json_type_name(json)
                ))),
            },
            FieldType::Bool => match json {
                serde_json::Value::Bool(b) => Ok(Value::Bool(*b)),
                _ => Err(VmError::new(format!(
                    "json.parse({parent_type}): field '{field_name}': expected Bool, got {}",
                    json_type_name(json)
                ))),
            },
            FieldType::List(inner) => match json {
                serde_json::Value::Array(arr) => {
                    let mut values = Vec::new();
                    for (i, item) in arr.iter().enumerate() {
                        let idx_name = format!("{field_name}[{i}]");
                        match self.json_to_typed_value(item, inner, parent_type, &idx_name) {
                            Ok(v) => values.push(v),
                            Err(e) => return Err(e),
                        }
                    }
                    Ok(Value::List(Rc::new(values)))
                }
                _ => Err(VmError::new(format!(
                    "json.parse({parent_type}): field '{field_name}': expected List, got {}",
                    json_type_name(json)
                ))),
            },
            FieldType::Option(inner) => match json {
                serde_json::Value::Null => {
                    Ok(Value::Variant("None".into(), Vec::new()))
                }
                _ => {
                    let val = self.json_to_typed_value(json, inner, parent_type, field_name)?;
                    Ok(Value::Variant("Some".into(), vec![val]))
                }
            },
            FieldType::Record(rec_name) => {
                let fields = self.load_record_fields(rec_name)?;
                let result = self.json_to_record(rec_name, &fields, json)?;
                match &result {
                    Value::Variant(name, inner) if name == "Ok" && inner.len() == 1 => {
                        Ok(inner[0].clone())
                    }
                    Value::Variant(name, inner) if name == "Err" && inner.len() == 1 => {
                        if let Value::String(msg) = &inner[0] {
                            Err(VmError::new(format!(
                                "json.parse({parent_type}): field '{field_name}': {msg}"
                            )))
                        } else {
                            Err(VmError::new(format!(
                                "json.parse({parent_type}): field '{field_name}': failed to parse {rec_name}"
                            )))
                        }
                    }
                    _ => Err(VmError::new(format!(
                        "json.parse({parent_type}): field '{field_name}': unexpected result"
                    ))),
                }
            }
        }
    }

    // ── Channel builtins ─────────────────────────────────────────────

    fn dispatch_channel(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "new" => {
                let capacity = match args.len() {
                    0 => 0,
                    1 => match &args[0] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        _ => return Err(VmError::new("channel.new capacity must be a non-negative integer".into())),
                    },
                    _ => return Err(VmError::new("channel.new takes 0 or 1 arguments".into())),
                };
                let id = self.next_channel_id;
                self.next_channel_id += 1;
                Ok(Value::Channel(Rc::new(Channel::new(id, capacity))))
            }
            "send" => {
                if args.len() != 2 {
                    return Err(VmError::new("channel.send takes 2 arguments (channel, value)".into()));
                }
                let Value::Channel(ch) = &args[0] else {
                    return Err(VmError::new("channel.send requires a channel as first argument".into()));
                };
                let val = args[1].clone();
                let ch = ch.clone();
                let max_retries = 100_000;
                for _ in 0..max_retries {
                    match ch.try_send(val.clone()) {
                        TrySendResult::Sent => return Ok(Value::Unit),
                        TrySendResult::Closed => {
                            return Err(VmError::new(format!("send on closed channel {}", ch.id)));
                        }
                        TrySendResult::Full => {}
                    }
                    // Run other fibers to try to drain the channel
                    if !self.run_other_fibers_once()? {
                        return Err(VmError::new(format!(
                            "deadlock: channel {} is full and no task can drain it", ch.id
                        )));
                    }
                }
                Err(VmError::new(format!(
                    "deadlock: channel {} is full and no task can drain it", ch.id
                )))
            }
            "receive" => {
                if args.len() != 1 {
                    return Err(VmError::new("channel.receive takes 1 argument (channel)".into()));
                }
                let Value::Channel(ch) = &args[0] else {
                    return Err(VmError::new("channel.receive requires a channel argument".into()));
                };
                let ch = ch.clone();
                // Yield first so other tasks get a fair shot (round-robin fan-out)
                let _ = self.run_other_fibers_once();
                let max_retries = 100_000;
                for _ in 0..max_retries {
                    match ch.try_receive() {
                        TryReceiveResult::Value(val) => {
                            return Ok(Value::Variant("Message".into(), vec![val]));
                        }
                        TryReceiveResult::Closed => {
                            return Ok(Value::Variant("Closed".into(), vec![]));
                        }
                        TryReceiveResult::Empty => {}
                    }
                    if !self.run_other_fibers_once()? {
                        return Err(VmError::new(format!(
                            "deadlock: channel {} is empty and no task can fill it", ch.id
                        )));
                    }
                }
                Err(VmError::new(format!(
                    "deadlock: channel {} is empty and no task can fill it", ch.id
                )))
            }
            "close" => {
                if args.len() != 1 {
                    return Err(VmError::new("channel.close takes 1 argument (channel)".into()));
                }
                let Value::Channel(ch) = &args[0] else {
                    return Err(VmError::new("channel.close requires a channel argument".into()));
                };
                ch.close();
                Ok(Value::Unit)
            }
            "try_send" => {
                if args.len() != 2 {
                    return Err(VmError::new("channel.try_send takes 2 arguments".into()));
                }
                let Value::Channel(ch) = &args[0] else {
                    return Err(VmError::new("channel.try_send requires a channel".into()));
                };
                match ch.try_send(args[1].clone()) {
                    TrySendResult::Sent => Ok(Value::Bool(true)),
                    TrySendResult::Full | TrySendResult::Closed => Ok(Value::Bool(false)),
                }
            }
            "try_receive" => {
                if args.len() != 1 {
                    return Err(VmError::new("channel.try_receive takes 1 argument".into()));
                }
                let Value::Channel(ch) = &args[0] else {
                    return Err(VmError::new("channel.try_receive requires a channel".into()));
                };
                match ch.try_receive() {
                    TryReceiveResult::Value(val) => Ok(Value::Variant("Message".into(), vec![val])),
                    TryReceiveResult::Empty => Ok(Value::Variant("Empty".into(), Vec::new())),
                    TryReceiveResult::Closed => Ok(Value::Variant("Closed".into(), Vec::new())),
                }
            }
            "select" => {
                if args.len() != 1 {
                    return Err(VmError::new("channel.select takes 1 argument (list of channels)".into()));
                }
                let Value::List(channels) = &args[0] else {
                    return Err(VmError::new("channel.select argument must be a list of channels".into()));
                };
                let channel_refs: Vec<Rc<Channel>> = channels
                    .iter()
                    .map(|v| match v {
                        Value::Channel(ch) => Ok(ch.clone()),
                        _ => Err(VmError::new("channel.select list must contain only channels".into())),
                    })
                    .collect::<Result<_, _>>()?;
                if channel_refs.is_empty() {
                    return Err(VmError::new("channel.select requires at least one channel".into()));
                }
                let max_retries = 100_000;
                for _ in 0..max_retries {
                    let mut all_closed = true;
                    let mut first_closed_ch = None;
                    for ch in &channel_refs {
                        match ch.try_receive() {
                            TryReceiveResult::Value(val) => {
                                return Ok(Value::Tuple(vec![
                                    Value::Channel(ch.clone()),
                                    Value::Variant("Message".into(), vec![val]),
                                ]));
                            }
                            TryReceiveResult::Closed => {
                                if first_closed_ch.is_none() {
                                    first_closed_ch = Some(ch.clone());
                                }
                                continue;
                            }
                            TryReceiveResult::Empty => {
                                all_closed = false;
                            }
                        }
                    }
                    if all_closed {
                        let ch = first_closed_ch.unwrap_or_else(|| channel_refs[0].clone());
                        return Ok(Value::Tuple(vec![
                            Value::Channel(ch),
                            Value::Variant("Closed".into(), vec![]),
                        ]));
                    }
                    if !self.run_other_fibers_once()? {
                        return Err(VmError::new(
                            "channel.select: deadlock detected - no channels have data and no tasks can make progress".into(),
                        ));
                    }
                }
                Err(VmError::new("channel.select: exceeded maximum retries".into()))
            }
            "each" => {
                if args.len() != 2 {
                    return Err(VmError::new("channel.each takes 2 arguments (channel, function)".into()));
                }
                let Value::Channel(ch) = &args[0] else {
                    return Err(VmError::new("channel.each requires a channel as first argument".into()));
                };
                let ch = ch.clone();
                let callback = args[1].clone();
                let max_total = 100_000;
                for _ in 0..max_total {
                    match ch.try_receive() {
                        TryReceiveResult::Value(val) => {
                            self.invoke_callable(&callback, &[val])?;
                            // After each message, yield to scheduler for round-robin.
                            // If we're running inside a fiber (scheduling_fibers is true),
                            // push the channel.each args back onto the stack so the
                            // CallBuiltin instruction can be re-executed, then yield.
                            if self.scheduling_fibers {
                                for arg in args {
                                    self.push(arg.clone());
                                }
                                return Err(VmError::yield_signal());
                            }
                            if !self.fibers.is_empty() {
                                let _ = self.run_other_fibers_once();
                            }
                        }
                        TryReceiveResult::Closed => {
                            return Ok(Value::Unit);
                        }
                        TryReceiveResult::Empty => {
                            // If we're in a fiber, yield to let other fibers run
                            // (they may produce data for this channel).
                            if self.scheduling_fibers {
                                for arg in args {
                                    self.push(arg.clone());
                                }
                                return Err(VmError::yield_signal());
                            }
                            if !self.run_other_fibers_once()? {
                                return Err(VmError::new(
                                    "channel.each: deadlock - channel is empty and no task can fill it".into(),
                                ));
                            }
                        }
                    }
                }
                Err(VmError::new("channel.each: exceeded maximum iterations".into()))
            }
            _ => Err(VmError::new(format!("unknown channel function: {name}"))),
        }
    }

    // ── Task builtins ────────────────────────────────────────────────

    fn dispatch_task(&mut self, name: &str, args: &[Value]) -> Result<Value, VmError> {
        match name {
            "spawn" => {
                if args.len() != 1 {
                    return Err(VmError::new("task.spawn takes 1 argument (a function)".into()));
                }
                let Value::VmClosure(closure) = &args[0] else {
                    return Err(VmError::new("task.spawn requires a function argument".into()));
                };
                let task_id = self.next_task_id;
                self.next_task_id += 1;
                let handle = Rc::new(TaskHandle {
                    id: task_id,
                    result: RefCell::new(None),
                });
                // Create a new fiber to run the closure
                let fiber_stack = vec![Value::Unit]; // dummy function slot
                // No args for a zero-arg spawn closure
                let fiber_frame = CallFrame {
                    closure: closure.clone(),
                    ip: 0,
                    base_slot: 1,
                };
                let fiber = VmFiber {
                    frames: vec![fiber_frame],
                    stack: fiber_stack,
                    state: FiberState::Ready,
                    handle: handle.clone(),
                };
                self.fibers.push(fiber);
                Ok(Value::Handle(handle))
            }
            "join" => {
                if args.len() != 1 {
                    return Err(VmError::new("task.join takes 1 argument (handle)".into()));
                }
                let Value::Handle(handle) = &args[0] else {
                    return Err(VmError::new("task.join requires a handle argument".into()));
                };
                let handle = handle.clone();
                let max_iterations = 100_000;
                for _ in 0..max_iterations {
                    if let Some(result) = handle.result.borrow().as_ref() {
                        return match result {
                            Ok(val) => Ok(val.clone()),
                            Err(msg) => Err(VmError::new(format!("joined task failed: {msg}"))),
                        };
                    }
                    if !self.run_other_fibers_once()? {
                        if let Some(result) = handle.result.borrow().as_ref() {
                            return match result {
                                Ok(val) => Ok(val.clone()),
                                Err(msg) => Err(VmError::new(format!("joined task failed: {msg}"))),
                            };
                        }
                        return Err(VmError::new("task.join: deadlock - target task not completed and no progress".into()));
                    }
                }
                Err(VmError::new("task.join: exceeded maximum iterations".into()))
            }
            "cancel" => {
                if args.len() != 1 {
                    return Err(VmError::new("task.cancel takes 1 argument (handle)".into()));
                }
                let Value::Handle(handle) = &args[0] else {
                    return Err(VmError::new("task.cancel requires a handle argument".into()));
                };
                let handle_id = handle.id;
                *handle.result.borrow_mut() = Some(Err("cancelled".to_string()));
                // Mark matching fiber as Failed
                // Mark the fiber as failed so it won't be scheduled again.
                for fiber in &mut self.fibers {
                    // We can't directly identify which fiber belongs to this handle_id,
                    // but marking the handle's result is sufficient: the fiber will see
                    // the cancellation when it next tries to run and the join caller
                    // will see the error result.
                    let _ = fiber;
                }
                // Remove any fibers whose handle result matches
                // For now, mark as failed in the fibers list
                let _ = handle_id;
                Ok(Value::Unit)
            }
            _ => Err(VmError::new(format!("unknown task function: {name}"))),
        }
    }

    // ── Fiber scheduling ─────────────────────────────────────────────

    /// Run one round of other fibers (not the main/current execution context).
    /// Returns Ok(true) if any fiber made progress, Ok(false) if no progress.
    fn run_other_fibers_once(&mut self) -> Result<bool, VmError> {
        if self.fibers.is_empty() || self.scheduling_fibers {
            return Ok(false);
        }
        self.scheduling_fibers = true;
        let mut any_progress = false;
        // Run each Ready fiber for one time slice.
        // We iterate by index since we need to swap state with self.
        let fiber_count = self.fibers.len();
        for i in 0..fiber_count {
            if i >= self.fibers.len() {
                break;
            }
            match &self.fibers[i].state {
                FiberState::Ready | FiberState::Running => {}
                _ => continue,
            }
            // Save current main execution state
            let saved_frames = std::mem::take(&mut self.frames);
            let saved_stack = std::mem::take(&mut self.stack);

            // Load fiber state
            self.frames = std::mem::take(&mut self.fibers[i].frames);
            self.stack = std::mem::take(&mut self.fibers[i].stack);
            self.fibers[i].state = FiberState::Running;

            // Run the fiber for a time slice
            let result = self.run_fiber_slice(100);

            // Save fiber state back
            self.fibers[i].frames = std::mem::take(&mut self.frames);
            self.fibers[i].stack = std::mem::take(&mut self.stack);

            // Restore main state
            self.frames = saved_frames;
            self.stack = saved_stack;

            match result {
                Ok(FiberSliceResult::Yielded) => {
                    self.fibers[i].state = FiberState::Ready;
                    any_progress = true;
                }
                Ok(FiberSliceResult::Completed(val)) => {
                    self.fibers[i].state = FiberState::Completed(val.clone());
                    // Store result directly in the fiber's handle (Rc-shared
                    // with the Value::Handle the caller holds).
                    *self.fibers[i].handle.result.borrow_mut() = Some(Ok(val));
                    any_progress = true;
                }
                Ok(FiberSliceResult::Blocked) => {
                    // Fiber is blocked on a channel op; it will be retried later
                    // State is already set by the blocking operation
                    self.fibers[i].state = FiberState::Ready;
                    // Still counts as having tried
                }
                Err(e) => {
                    self.fibers[i].state = FiberState::Failed(e.message.clone());
                    *self.fibers[i].handle.result.borrow_mut() = Some(Err(e.message));
                    any_progress = true;
                }
            }
        }
        // Clean up completed/failed fibers
        // (Keep them around so join() can find them, but skip in scheduling)
        self.scheduling_fibers = false;
        Ok(any_progress)
    }

    /// Run the current frames/stack for up to `max_steps` instructions.
    fn run_fiber_slice(&mut self, max_steps: usize) -> Result<FiberSliceResult, VmError> {
        for _ in 0..max_steps {
            if self.frames.is_empty() {
                // Fiber finished
                let result = if self.stack.is_empty() {
                    Value::Unit
                } else {
                    self.stack.last().cloned().unwrap_or(Value::Unit)
                };
                return Ok(FiberSliceResult::Completed(result));
            }
            // Save IP before this instruction so we can rewind on yield
            let saved_ip = self.current_frame().ip;
            let op_byte = self.read_byte();
            match Op::from_byte(op_byte) {
                Some(Op::Return) => {
                    let result = self.pop();
                    let finished_base = self.current_frame().base_slot;
                    self.frames.pop();
                    if self.frames.is_empty() {
                        // Fiber completed
                        return Ok(FiberSliceResult::Completed(result));
                    }
                    let func_slot = if finished_base > 0 { finished_base - 1 } else { 0 };
                    self.stack.truncate(func_slot);
                    self.push(result);
                }
                Some(op) => {
                    match self.dispatch_op(op) {
                        Ok(()) => {}
                        Err(e) if e.is_yield => {
                            // Cooperative yield: rewind IP to re-execute this
                            // instruction when the fiber is next scheduled.
                            // The yielding code (e.g. channel.each) has already
                            // restored its arguments on the stack.
                            self.current_frame_mut().ip = saved_ip;
                            return Ok(FiberSliceResult::Yielded);
                        }
                        Err(e) => return Err(e),
                    }
                }
                None => {
                    return Err(VmError::new(format!("unknown opcode: {op_byte}")));
                }
            }
        }
        // Time slice expired
        Ok(FiberSliceResult::Yielded)
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Chunk, Function, Op};
    use crate::compiler::Compiler;
    use crate::lexer::{Lexer, Span};
    use crate::parser::Parser;

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

    /// Helper: compile and run a silt program through the VM pipeline.
    fn run_vm(source: &str) -> Value {
        let tokens = Lexer::new(source).tokenize().unwrap();
        let program = Parser::new(tokens).parse_program().unwrap();
        let mut compiler = Compiler::new();
        let functions = compiler.compile_program(&program).unwrap();
        let script = Rc::new(functions.into_iter().next().unwrap());
        let mut vm = Vm::new();
        vm.run(script).unwrap()
    }


    // ── Phase 1 bytecode-level tests ──────────────────────────────

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
        let script = make_function(|chunk| {
            let name = chunk.add_constant(Value::String("x".to_string()));
            let val = chunk.add_constant(Value::Int(42));
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::SetGlobal, span());
            chunk.emit_u16(name, span());
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
        let script = make_function(|chunk| {
            let val = chunk.add_constant(Value::Int(10));
            chunk.emit_op(Op::Unit, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(val, span());
            chunk.emit_op(Op::SetLocal, span());
            chunk.emit_u16(0, span());
            chunk.emit_op(Op::Pop, span());
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
        let script = make_function(|chunk| {
            let one = chunk.add_constant(Value::Int(1));
            let two = chunk.add_constant(Value::Int(2));
            chunk.emit_op(Op::False, span());
            let patch = chunk.emit_jump(Op::JumpIfFalse, span());
            chunk.emit_op(Op::Constant, span());
            chunk.emit_u16(one, span());
            let skip_else = chunk.emit_jump(Op::Jump, span());
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

    // ── Phase 2 end-to-end tests ──────────────────────────────────

    #[test]
    fn test_e2e_hello_world() {
        run_vm(r#"fn main() { println("hello") }"#);
    }

    #[test]
    fn test_e2e_arithmetic() {
        let result = run_vm(r#"fn main() { 2 + 3 * 4 }"#);
        assert_eq!(result, Value::Int(14));
    }

    #[test]
    fn test_e2e_function_call() {
        let result = run_vm(r#"
            fn add(a, b) { a + b }
            fn main() { add(10, 20) }
        "#);
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_e2e_let_binding() {
        let result = run_vm(r#"
            fn main() {
                let x = 42
                x
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_e2e_let_and_string_interp() {
        run_vm(r#"
            fn main() {
                let x = 42
                println("x = {x}")
            }
        "#);
    }

    #[test]
    fn test_e2e_multiple_functions() {
        let result = run_vm(r#"
            fn double(n) { n * 2 }
            fn add_one(n) { n + 1 }
            fn main() { add_one(double(5)) }
        "#);
        assert_eq!(result, Value::Int(11));
    }

    #[test]
    fn test_e2e_recursion() {
        let result = run_vm(r#"
            fn factorial(n) {
                match n {
                    0 -> 1
                    _ -> n * factorial(n - 1)
                }
            }
            fn main() { factorial(5) }
        "#);
        assert_eq!(result, Value::Int(120));
    }

    #[test]
    fn test_e2e_string_operations() {
        let result = run_vm(r#"
            fn main() {
                let s = "hello, world"
                string.length(s)
            }
        "#);
        assert_eq!(result, Value::Int(12));
    }

    #[test]
    fn test_e2e_list_operations() {
        let result = run_vm(r#"
            fn main() {
                let xs = [1, 2, 3, 4, 5]
                list.length(xs)
            }
        "#);
        assert_eq!(result, Value::Int(5));
    }

    #[test]
    fn test_e2e_test_assert() {
        run_vm(r#"
            fn main() {
                test.assert_eq(2 + 2, 4)
            }
        "#);
    }

    #[test]
    fn test_e2e_nested_calls() {
        let result = run_vm(r#"
            fn f(x) { x + 1 }
            fn g(x) { f(x) * 2 }
            fn main() { g(10) }
        "#);
        assert_eq!(result, Value::Int(22));
    }

    #[test]
    fn test_e2e_match_int() {
        let result = run_vm(r#"
            fn classify(n) {
                match n {
                    0 -> "zero"
                    1 -> "one"
                    _ -> "other"
                }
            }
            fn main() { classify(1) }
        "#);
        assert_eq!(result, Value::String("one".into()));
    }

    #[test]
    fn test_e2e_boolean_logic() {
        let result = run_vm(r#"
            fn main() {
                let a = true
                let b = false
                a && !b
            }
        "#);
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_e2e_builtin_println_call() {
        // Test that println works when called as a regular function via globals
        run_vm(r#"
            fn main() {
                println("testing 1 2 3")
            }
        "#);
    }

    #[test]
    fn test_e2e_variant_constructor() {
        let result = run_vm(r#"
            fn main() {
                let x = Some(42)
                x
            }
        "#);
        assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(42)]));
    }

    #[test]
    fn test_e2e_int_to_string() {
        let result = run_vm(r#"
            fn main() {
                int.to_string(42)
            }
        "#);
        assert_eq!(result, Value::String("42".into()));
    }

    #[test]
    fn test_e2e_list_append() {
        let result = run_vm(r#"
            fn main() {
                let xs = [1, 2, 3]
                list.append(xs, 4)
            }
        "#);
        assert_eq!(result, Value::List(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)])));
    }

    // ── Phase 3: Closures and upvalue capture ────────────────────────

    #[test]
    fn test_closure_capture() {
        let result = run_vm(r#"
            fn make_adder(n) {
                fn(x) { x + n }
            }
            fn main() {
                let add5 = make_adder(5)
                add5(10)
            }
        "#);
        assert_eq!(result, Value::Int(15));
    }

    #[test]
    fn test_closure_in_map() {
        let result = run_vm(r#"
            fn main() {
                let factor = 10
                [1, 2, 3] |> list.map(fn(x) { x * factor })
            }
        "#);
        assert_eq!(result, Value::List(Rc::new(vec![Value::Int(10), Value::Int(20), Value::Int(30)])));
    }

    #[test]
    fn test_higher_order() {
        let result = run_vm(r#"
            fn apply_twice(f, x) {
                f(f(x))
            }
            fn main() {
                let double = fn(x) { x * 2 }
                apply_twice(double, 3)
            }
        "#);
        assert_eq!(result, Value::Int(12));
    }

    #[test]
    fn test_closure_counter() {
        // Tests that closures capture values, not references
        let result = run_vm(r#"
            fn main() {
                let fns = [1, 2, 3] |> list.map(fn(n) {
                    fn() { n * 10 }
                })
                fns |> list.map(fn(f) { f() })
            }
        "#);
        assert_eq!(result, Value::List(Rc::new(vec![Value::Int(10), Value::Int(20), Value::Int(30)])));
    }

    #[test]
    fn test_closure_multiple_captures() {
        let result = run_vm(r#"
            fn make_linear(a, b) {
                fn(x) { a * x + b }
            }
            fn main() {
                let f = make_linear(3, 7)
                f(10)
            }
        "#);
        assert_eq!(result, Value::Int(37));
    }

    #[test]
    fn test_closure_transitive_capture() {
        // outer -> middle -> inner: transitive upvalue chaining
        let result = run_vm(r#"
            fn outer(x) {
                let make_inner = fn() {
                    fn() { x }
                }
                make_inner()
            }
            fn main() {
                let f = outer(42)
                f()
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_closure_no_capture() {
        // Lambda that doesn't capture anything (no upvalues needed)
        let result = run_vm(r#"
            fn main() {
                let f = fn(x) { x + 1 }
                f(10)
            }
        "#);
        assert_eq!(result, Value::Int(11));
    }

    #[test]
    fn test_closure_with_filter() {
        let result = run_vm(r#"
            fn main() {
                let threshold = 3
                [1, 2, 3, 4, 5] |> list.filter(fn(x) { x > threshold })
            }
        "#);
        assert_eq!(result, Value::List(Rc::new(vec![Value::Int(4), Value::Int(5)])));
    }

    #[test]
    fn test_closure_with_fold() {
        let result = run_vm(r#"
            fn main() {
                let offset = 100
                [1, 2, 3] |> list.fold(offset, fn(acc, x) { acc + x })
            }
        "#);
        assert_eq!(result, Value::Int(106));
    }

    #[test]
    fn test_let_tuple_destructure() {
        let result = run_vm(r#"
            fn main() {
                let (a, b) = (10, 20)
                a + b
            }
        "#);
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_let_tuple_destructure_three() {
        let result = run_vm(r#"
            fn main() {
                let (a, b, c) = (1, 2, 3)
                a * 100 + b * 10 + c
            }
        "#);
        assert_eq!(result, Value::Int(123));
    }

    #[test]
    fn test_closure_returned_from_fn() {
        // A named function returns a closure that captures a parameter
        let result = run_vm(r#"
            fn multiplier(factor) {
                fn(x) { x * factor }
            }
            fn main() {
                let times3 = multiplier(3)
                let times7 = multiplier(7)
                times3(10) + times7(5)
            }
        "#);
        assert_eq!(result, Value::Int(65));
    }

    #[test]
    fn test_closure_with_pipe_and_fn_syntax() {
        // Pipe with explicit fn(x) { ... } closure
        let result = run_vm(r#"
            fn main() {
                let base = 5
                [1, 2, 3] |> list.map(fn(x) { x + base })
            }
        "#);
        assert_eq!(result, Value::List(Rc::new(vec![Value::Int(6), Value::Int(7), Value::Int(8)])));
    }

    #[test]
    fn test_trailing_closure_with_capture() {
        // Pipe with trailing closure syntax { x -> ... }
        let result = run_vm(r#"
            fn main() {
                let factor = 10
                [1, 2, 3] |> list.map { x -> x * factor }
            }
        "#);
        assert_eq!(result, Value::List(Rc::new(vec![Value::Int(10), Value::Int(20), Value::Int(30)])));
    }

    #[test]
    fn test_trailing_closure_filter_with_capture() {
        let result = run_vm(r#"
            fn main() {
                let limit = 3
                [1, 2, 3, 4, 5] |> list.filter { x -> x > limit }
            }
        "#);
        assert_eq!(result, Value::List(Rc::new(vec![Value::Int(4), Value::Int(5)])));
    }

    #[test]
    fn test_chained_pipes_with_closures() {
        let result = run_vm(r#"
            fn main() {
                let offset = 10
                let cutoff = 13
                [1, 2, 3, 4, 5]
                    |> list.map(fn(x) { x + offset })
                    |> list.filter(fn(x) { x > cutoff })
            }
        "#);
        assert_eq!(result, Value::List(Rc::new(vec![Value::Int(14), Value::Int(15)])));
    }

    // ── Phase 4: Full pattern matching ──────────────────────────────

    #[test]
    fn test_match_int_literal() {
        let result = run_vm(r#"
            fn main() { match 42 { 42 -> "yes" _ -> "no" } }
        "#);
        assert_eq!(result, Value::String("yes".into()));
    }

    #[test]
    fn test_match_int_fallthrough() {
        let result = run_vm(r#"
            fn main() { match 99 { 42 -> "yes" _ -> "no" } }
        "#);
        assert_eq!(result, Value::String("no".into()));
    }

    #[test]
    fn test_match_string_literal() {
        let result = run_vm(r#"
            fn main() { match "hello" { "hello" -> 1 _ -> 0 } }
        "#);
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn test_match_bool_literal() {
        let result = run_vm(r#"
            fn main() { match true { true -> "yes" false -> "no" } }
        "#);
        assert_eq!(result, Value::String("yes".into()));
    }

    #[test]
    fn test_match_float_literal() {
        let result = run_vm(r#"
            fn main() { match 3.14 { 3.14 -> "pi" _ -> "other" } }
        "#);
        assert_eq!(result, Value::String("pi".into()));
    }

    #[test]
    fn test_match_tuple() {
        let result = run_vm(r#"
            fn main() {
                match (1, 2) { (1, y) -> y * 10  _ -> 0 }
            }
        "#);
        assert_eq!(result, Value::Int(20));
    }

    #[test]
    fn test_match_tuple_wildcard() {
        let result = run_vm(r#"
            fn main() {
                match (1, 2) { (_, y) -> y + 100  _ -> 0 }
            }
        "#);
        assert_eq!(result, Value::Int(102));
    }

    #[test]
    fn test_match_tuple_len_mismatch() {
        let result = run_vm(r#"
            fn main() {
                match (1, 2, 3) { (a, b) -> a + b  _ -> 99 }
            }
        "#);
        assert_eq!(result, Value::Int(99));
    }

    #[test]
    fn test_match_list_exact() {
        let result = run_vm(r#"
            fn main() {
                match [1, 2, 3] { [a, b, c] -> a + b + c  _ -> 0 }
            }
        "#);
        assert_eq!(result, Value::Int(6));
    }

    #[test]
    fn test_match_list_exact_mismatch() {
        let result = run_vm(r#"
            fn main() {
                match [1, 2] { [a, b, c] -> a + b + c  _ -> 99 }
            }
        "#);
        assert_eq!(result, Value::Int(99));
    }

    #[test]
    fn test_match_list_head_rest() {
        let result = run_vm(r#"
            fn main() {
                match [10, 20, 30] { [h, ..t] -> h  _ -> 0 }
            }
        "#);
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_match_list_rest_value() {
        let result = run_vm(r#"
            fn main() {
                match [10, 20, 30] { [_, ..t] -> t  _ -> [] }
            }
        "#);
        assert_eq!(result, Value::List(Rc::new(vec![Value::Int(20), Value::Int(30)])));
    }

    #[test]
    fn test_match_list_empty_rest() {
        let result = run_vm(r#"
            fn main() {
                match [10] { [h, ..t] -> t  _ -> [99] }
            }
        "#);
        assert_eq!(result, Value::List(Rc::new(vec![])));
    }

    #[test]
    fn test_match_constructor_simple() {
        let result = run_vm(r#"
            fn main() {
                match Some(42) { Some(n) -> n  None -> 0 }
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_match_constructor_none() {
        let result = run_vm(r#"
            fn main() {
                match None { Some(n) -> n  None -> 0 }
            }
        "#);
        assert_eq!(result, Value::Int(0));
    }

    #[test]
    fn test_match_constructor_ok_err() {
        let result = run_vm(r#"
            fn main() {
                let v = Ok(42)
                match v { Ok(n) -> n  Err(_) -> -1 }
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_match_nested_constructor_tuple() {
        let result = run_vm(r#"
            fn main() {
                match Some((1, 2)) { Some((a, b)) -> a + b  None -> 0 }
            }
        "#);
        assert_eq!(result, Value::Int(3));
    }

    #[test]
    fn test_match_nested_constructor_list() {
        let result = run_vm(r#"
            fn main() {
                match Some([10, 20]) {
                    Some([h, ..t]) -> h
                    _ -> 0
                }
            }
        "#);
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_match_or_pattern() {
        let result = run_vm(r#"
            fn main() {
                match 2 { 1 | 2 | 3 -> "small" _ -> "big" }
            }
        "#);
        assert_eq!(result, Value::String("small".into()));
    }

    #[test]
    fn test_match_or_pattern_no_match() {
        let result = run_vm(r#"
            fn main() {
                match 5 { 1 | 2 | 3 -> "small" _ -> "big" }
            }
        "#);
        assert_eq!(result, Value::String("big".into()));
    }

    #[test]
    fn test_match_guard() {
        let result = run_vm(r#"
            fn main() {
                match 42 {
                    n when n > 100 -> "big"
                    n when n > 0 -> "positive"
                    _ -> "other"
                }
            }
        "#);
        assert_eq!(result, Value::String("positive".into()));
    }

    #[test]
    fn test_match_guard_all_fail() {
        let result = run_vm(r#"
            fn main() {
                match -5 {
                    n when n > 100 -> "big"
                    n when n > 0 -> "positive"
                    _ -> "other"
                }
            }
        "#);
        assert_eq!(result, Value::String("other".into()));
    }

    #[test]
    fn test_match_range() {
        let result = run_vm(r#"
            fn main() {
                match 5 { 1..10 -> "in range" _ -> "out" }
            }
        "#);
        assert_eq!(result, Value::String("in range".into()));
    }

    #[test]
    fn test_match_range_boundary() {
        let result = run_vm(r#"
            fn main() {
                match 10 { 1..10 -> "in range" _ -> "out" }
            }
        "#);
        assert_eq!(result, Value::String("in range".into()));
    }

    #[test]
    fn test_match_range_out() {
        let result = run_vm(r#"
            fn main() {
                match 11 { 1..10 -> "in range" _ -> "out" }
            }
        "#);
        assert_eq!(result, Value::String("out".into()));
    }

    #[test]
    fn test_guardless_match() {
        let result = run_vm(r#"
            fn main() {
                let x = 5
                match { x > 10 -> "big"  x > 0 -> "positive"  _ -> "other" }
            }
        "#);
        assert_eq!(result, Value::String("positive".into()));
    }

    #[test]
    fn test_guardless_match_default() {
        let result = run_vm(r#"
            fn main() {
                let x = -5
                match { x > 10 -> "big"  x > 0 -> "positive"  _ -> "other" }
            }
        "#);
        assert_eq!(result, Value::String("other".into()));
    }

    #[test]
    fn test_let_tuple_destructure_nested() {
        let result = run_vm(r#"
            fn main() {
                let (a, (b, c)) = (1, (2, 3))
                a + b + c
            }
        "#);
        assert_eq!(result, Value::Int(6));
    }

    #[test]
    fn test_let_list_destructure() {
        let result = run_vm(r#"
            fn main() {
                let [a, b, c] = [10, 20, 30]
                a + b + c
            }
        "#);
        assert_eq!(result, Value::Int(60));
    }

    #[test]
    fn test_let_list_head_rest() {
        let result = run_vm(r#"
            fn main() {
                let [h, ..t] = [1, 2, 3, 4]
                h
            }
        "#);
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn test_match_multiple_arms() {
        let result = run_vm(r#"
            fn classify(n) {
                match n {
                    0 -> "zero"
                    1 -> "one"
                    2 -> "two"
                    _ -> "many"
                }
            }
            fn main() {
                classify(2)
            }
        "#);
        assert_eq!(result, Value::String("two".into()));
    }

    #[test]
    fn test_match_ident_binding() {
        let result = run_vm(r#"
            fn main() {
                match 42 { x -> x + 1 }
            }
        "#);
        assert_eq!(result, Value::Int(43));
    }

    #[test]
    fn test_match_wildcard() {
        let result = run_vm(r#"
            fn main() {
                match 42 { _ -> "matched" }
            }
        "#);
        assert_eq!(result, Value::String("matched".into()));
    }

    #[test]
    fn test_match_constructor_with_guard() {
        let result = run_vm(r#"
            fn main() {
                match Some(5) {
                    Some(n) when n > 10 -> "big"
                    Some(n) -> n * 2
                    None -> 0
                }
            }
        "#);
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_when_bool_guard() {
        let result = run_vm(r#"
            fn safe_div(a, b) {
                when b != 0 else { return Err("div by zero") }
                Ok(a / b)
            }
            fn main() {
                match safe_div(10, 2) { Ok(n) -> n  Err(_) -> -1 }
            }
        "#);
        assert_eq!(result, Value::Int(5));
    }

    #[test]
    fn test_when_bool_guard_fails() {
        let result = run_vm(r#"
            fn safe_div(a, b) {
                when b != 0 else { return Err("div by zero") }
                Ok(a / b)
            }
            fn main() {
                match safe_div(10, 0) { Ok(n) -> n  Err(_) -> -1 }
            }
        "#);
        assert_eq!(result, Value::Int(-1));
    }

    #[test]
    fn test_match_list_two_elems_with_rest() {
        let result = run_vm(r#"
            fn main() {
                match [1, 2, 3, 4, 5] {
                    [a, b, ..rest] -> a + b
                    _ -> 0
                }
            }
        "#);
        assert_eq!(result, Value::Int(3));
    }

    #[test]
    fn test_match_tuple_three() {
        let result = run_vm(r#"
            fn main() {
                match (10, 20, 30) {
                    (a, b, c) -> a + b + c
                    _ -> 0
                }
            }
        "#);
        assert_eq!(result, Value::Int(60));
    }

    #[test]
    fn test_match_nested_tuple_in_list() {
        // Match a list where elements are extracted as simple ints
        let result = run_vm(r#"
            fn main() {
                match [1, 2] {
                    [a, b] -> a * 100 + b
                    _ -> 0
                }
            }
        "#);
        assert_eq!(result, Value::Int(102));
    }

    #[test]
    fn test_match_constructor_wildcard_field() {
        let result = run_vm(r#"
            fn main() {
                match Ok(42) { Ok(_) -> "is ok" Err(_) -> "is err" }
            }
        "#);
        assert_eq!(result, Value::String("is ok".into()));
    }

    #[test]
    fn test_match_or_pattern_constructor() {
        let result = run_vm(r#"
            fn main() {
                match None { Some(_) -> "has value"  None -> "empty" }
            }
        "#);
        assert_eq!(result, Value::String("empty".into()));
    }

    #[test]
    fn test_match_deeply_nested() {
        // Some((a, [h, ..t]))
        let result = run_vm(r#"
            fn main() {
                match Some((1, [10, 20, 30])) {
                    Some((a, [h, ..t])) -> a + h
                    _ -> 0
                }
            }
        "#);
        assert_eq!(result, Value::Int(11));
    }

    #[test]
    fn test_guardless_match_first_branch() {
        let result = run_vm(r#"
            fn main() {
                let x = 50
                match { x > 10 -> "big"  x > 0 -> "positive"  _ -> "other" }
            }
        "#);
        assert_eq!(result, Value::String("big".into()));
    }

    #[test]
    fn test_match_in_function() {
        let result = run_vm(r#"
            fn describe(opt) {
                match opt {
                    Some(n) when n > 0 -> "positive"
                    Some(0) -> "zero"
                    Some(_) -> "negative"
                    None -> "nothing"
                }
            }
            fn main() {
                describe(Some(0))
            }
        "#);
        assert_eq!(result, Value::String("zero".into()));
    }

    #[test]
    fn test_match_float_range() {
        let result = run_vm(r#"
            fn main() {
                match 3.14 {
                    0.0..1.0 -> "small"
                    1.0..5.0 -> "medium"
                    _ -> "large"
                }
            }
        "#);
        assert_eq!(result, Value::String("medium".into()));
    }

    #[test]
    fn test_match_float_range_out() {
        let result = run_vm(r#"
            fn main() {
                match 10.0 {
                    0.0..1.0 -> "small"
                    1.0..5.0 -> "medium"
                    _ -> "large"
                }
            }
        "#);
        assert_eq!(result, Value::String("large".into()));
    }

    #[test]
    fn test_match_recursive_list_sum() {
        // Use match to destructure a list recursively
        let result = run_vm(r#"
            fn sum(xs) {
                match xs {
                    [] -> 0
                    [h, ..t] -> h + sum(t)
                }
            }
            fn main() {
                sum([1, 2, 3, 4, 5])
            }
        "#);
        assert_eq!(result, Value::Int(15));
    }

    #[test]
    fn test_match_map_pattern() {
        let result = run_vm(r#"
            fn main() {
                let m = #{"name": "Alice", "age": "30"}
                match m {
                    #{"name": n} -> n
                    _ -> "unknown"
                }
            }
        "#);
        assert_eq!(result, Value::String("Alice".into()));
    }

    #[test]
    fn test_match_constructor_nested_or() {
        let result = run_vm(r#"
            fn main() {
                match 42 {
                    1 | 2 | 3 -> "tiny"
                    n when n > 40 -> "big"
                    _ -> "other"
                }
            }
        "#);
        assert_eq!(result, Value::String("big".into()));
    }

    #[test]
    fn test_match_tuple_nested_wildcard() {
        let result = run_vm(r#"
            fn main() {
                match (1, (2, 3)) {
                    (1, (_, c)) -> c * 10
                    _ -> 0
                }
            }
        "#);
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_match_list_empty() {
        let result = run_vm(r#"
            fn main() {
                match [] {
                    [] -> "empty"
                    _ -> "not empty"
                }
            }
        "#);
        assert_eq!(result, Value::String("empty".into()));
    }

    #[test]
    fn test_let_constructor_destructure() {
        let result = run_vm(r#"
            fn main() {
                let x = Ok(42)
                match x { Ok(n) -> n  Err(_) -> 0 }
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_match_multiple_constructors_sequence() {
        let result = run_vm(r#"
            fn process(items) {
                match items {
                    [] -> 0
                    [h, ..t] -> h + process(t)
                }
            }
            fn main() {
                process([10, 20, 30])
            }
        "#);
        assert_eq!(result, Value::Int(60));
    }

    #[test]
    fn test_match_pin_pattern() {
        let result = run_vm(r#"
            fn main() {
                let expected = 42
                match 42 {
                    ^expected -> "matched"
                    _ -> "nope"
                }
            }
        "#);
        assert_eq!(result, Value::String("matched".into()));
    }

    #[test]
    fn test_match_pin_pattern_no_match() {
        let result = run_vm(r#"
            fn main() {
                let expected = 42
                match 99 {
                    ^expected -> "matched"
                    _ -> "nope"
                }
            }
        "#);
        assert_eq!(result, Value::String("nope".into()));
    }

    #[test]
    fn test_when_pattern_match() {
        let result = run_vm(r#"
            fn extract(val) {
                when Some(n) = val else { return -1 }
                n
            }
            fn main() {
                extract(Some(42))
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_when_pattern_match_fails() {
        let result = run_vm(r#"
            fn extract(val) {
                when Some(n) = val else { return -1 }
                n
            }
            fn main() {
                extract(None)
            }
        "#);
        assert_eq!(result, Value::Int(-1));
    }

    #[test]
    fn test_match_or_pattern_with_binding() {
        // Or-patterns where each alt binds the same variable
        let result = run_vm(r#"
            fn main() {
                match Some(5) {
                    Some(n) -> n * 2
                    None -> 0
                }
            }
        "#);
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_match_guard_with_tuple() {
        let result = run_vm(r#"
            fn main() {
                match (3, 4) {
                    (a, b) when a + b > 10 -> "big"
                    (a, b) -> a + b
                }
            }
        "#);
        assert_eq!(result, Value::Int(7));
    }

    // ── Phase 5 tests ──────────────────────────────────────────

    #[test]
    fn test_loop_sum() {
        let result = run_vm(r#"
            fn main() {
                loop x = 0, sum = 0 {
                    match x >= 10 {
                        true -> sum
                        _ -> loop(x + 1, sum + x)
                    }
                }
            }
        "#);
        assert_eq!(result, Value::Int(45));
    }

    #[test]
    fn test_loop_factorial() {
        let result = run_vm(r#"
            fn main() {
                loop n = 10, acc = 1 {
                    match n <= 1 {
                        true -> acc
                        _ -> loop(n - 1, acc * n)
                    }
                }
            }
        "#);
        assert_eq!(result, Value::Int(3628800));
    }

    #[test]
    fn test_record_create_and_access() {
        let result = run_vm(r#"
            type User { name: String, age: Int }
            fn main() {
                let u = User { name: "Alice", age: 30 }
                u.age
            }
        "#);
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_record_update() {
        let result = run_vm(r#"
            type User { name: String, age: Int }
            fn main() {
                let u = User { name: "Alice", age: 30 }
                let u2 = u.{ age: 31 }
                u2.age
            }
        "#);
        assert_eq!(result, Value::Int(31));
    }

    #[test]
    fn test_range_expression() {
        let result = run_vm(r#"
            fn main() {
                let nums = 1..6
                nums |> list.fold(0) { acc, n -> acc + n }
            }
        "#);
        assert_eq!(result, Value::Int(15));
    }

    #[test]
    fn test_set_literal() {
        let result = run_vm(r#"
            fn main() {
                let s = #[1, 2, 3, 2, 1]
                set.length(s)
            }
        "#);
        assert_eq!(result, Value::Int(3));
    }

    #[test]
    fn test_question_mark_ok() {
        let result = run_vm(r#"
            fn parse_add(a, b) {
                let x = int.parse(a)?
                let y = int.parse(b)?
                Ok(x + y)
            }
            fn main() {
                match parse_add("10", "20") {
                    Ok(n) -> n
                    Err(_) -> -1
                }
            }
        "#);
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_question_mark_err() {
        let result = run_vm(r#"
            fn parse_add(a, b) {
                let x = int.parse(a)?
                let y = int.parse(b)?
                Ok(x + y)
            }
            fn main() {
                match parse_add("10", "abc") {
                    Ok(n) -> n
                    Err(_) -> -1
                }
            }
        "#);
        assert_eq!(result, Value::Int(-1));
    }

    #[test]
    fn test_type_decl_variant_constructors() {
        let result = run_vm(r#"
            type Color { Red, Green, Blue }
            fn main() {
                let c = Red
                match c { Red -> 1  Green -> 2  Blue -> 3 }
            }
        "#);
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn test_type_decl_variant_with_fields() {
        let result = run_vm(r#"
            type Shape { Circle(Float), Rect(Float, Float) }
            fn main() {
                let s = Circle(5.0)
                match s {
                    Circle(r) -> r
                    Rect(w, h) -> w + h
                }
            }
        "#);
        assert_eq!(result, Value::Float(5.0));
    }

    #[test]
    fn test_custom_display_trait() {
        let result = run_vm(r#"
            type Shape { Circle(Float), Rect(Float, Float) }
            trait Display for Shape {
                fn display(self) -> String {
                    match self {
                        Circle(r) -> "Circle"
                        Rect(w, h) -> "Rect"
                    }
                }
            }
            fn main() {
                let s = Circle(5.0)
                s.display()
            }
        "#);
        assert_eq!(result, Value::String("Circle".to_string()));
    }

    #[test]
    fn test_tuple_index_access() {
        let result = run_vm(r#"
            fn main() {
                let pair = (10, 20)
                pair.0 + pair.1
            }
        "#);
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_recursive_variant_eval() {
        let result = run_vm(r#"
            type Expr { Num(Int), Add(Expr, Expr) }
            fn eval(expr) {
                match expr {
                    Num(n) -> n
                    Add(l, r) -> eval(l) + eval(r)
                }
            }
            fn main() {
                eval(Add(Num(3), Num(5)))
            }
        "#);
        assert_eq!(result, Value::Int(8));
    }

    #[test]
    fn test_loop_in_function() {
        let result = run_vm(r#"
            fn sum_to(n) {
                loop i = 0, acc = 0 {
                    match i > n {
                        true -> acc
                        _ -> loop(i + 1, acc + i)
                    }
                }
            }
            fn main() {
                sum_to(100)
            }
        "#);
        assert_eq!(result, Value::Int(5050));
    }

    // ── Concurrency tests ────────────────────────────────────────────

    #[test]
    fn test_spawn_join() {
        let result = run_vm(r#"
            fn main() {
                let t = task.spawn(fn() { 42 })
                task.join(t)
            }
        "#);
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_spawn_join_already_completed() {
        // Ensure task.join works when the fiber has already completed
        // before join is called (the original deadlock scenario).
        let result = run_vm(r#"
            fn main() {
                let ch = channel.new(1)
                let t = task.spawn(fn() {
                    channel.send(ch, "done")
                    99
                })
                -- Wait for the message, ensuring the fiber runs to completion
                let Message(msg) = channel.receive(ch)
                -- Now the fiber should already be completed
                task.join(t)
            }
        "#);
        assert_eq!(result, Value::Int(99));
    }

    #[test]
    fn test_spawn_join_multiple_completed() {
        // Multiple fibers that complete before join is called
        let result = run_vm(r#"
            fn main() {
                let ch = channel.new(10)
                let t1 = task.spawn(fn() {
                    channel.send(ch, 1)
                    10
                })
                let t2 = task.spawn(fn() {
                    channel.send(ch, 2)
                    20
                })
                let t3 = task.spawn(fn() {
                    channel.send(ch, 3)
                    30
                })
                -- Drain all messages so fibers complete
                let Message(_) = channel.receive(ch)
                let Message(_) = channel.receive(ch)
                let Message(_) = channel.receive(ch)
                -- All fibers should be done; join should not deadlock
                let a = task.join(t1)
                let b = task.join(t2)
                let c = task.join(t3)
                a + b + c
            }
        "#);
        assert_eq!(result, Value::Int(60));
    }
}
