//! Bytecode definitions for Silt's stack-based VM.
//!
//! A `Function` is the compilation unit — it holds a `Chunk` of bytecode,
//! a constant pool, and source span mappings for error reporting.

use std::sync::Arc;

use crate::lexer::Span;
use crate::value::Value;

// ── Opcodes ────────────────────────────────────────────────────────

/// Bytecode instructions for the stack-based VM.
///
/// Operand encoding: all multi-byte operands are little-endian.
/// `u16` operands follow the opcode byte. `u8` operands are inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    // ── Constants & literals ────────────────────────────────────
    /// Push `constants[u16]` onto the stack.
    Constant, // operand: u16 index
    /// Push Unit.
    Unit,
    /// Push true.
    True,
    /// Push false.
    False,

    // ── Arithmetic ─────────────────────────────────────────────
    Add,
    Sub,
    Mul,
    Div,
    Mod,

    // ── Comparison ─────────────────────────────────────────────
    Eq,
    Neq,
    Lt,
    Gt,
    Leq,
    Geq,

    // ── Unary ──────────────────────────────────────────────────
    Negate,
    Not,

    // ── Logical ────────────────────────────────────────────────
    And,
    Or,

    // ── String interpolation ───────────────────────────────────
    /// Concatenate `u8` stringified values into one String.
    StringConcat, // operand: u8 count
    /// Convert TOS to its Display string.
    DisplayValue,

    // ── Variables ──────────────────────────────────────────────
    /// Push `stack[frame_base + u16]`.
    GetLocal, // operand: u16 slot
    /// Store TOS into `stack[frame_base + u16]`. Does NOT pop.
    SetLocal, // operand: u16 slot
    /// Push `globals[constants[u16]]`.
    GetGlobal, // operand: u16 name_index
    /// Store TOS into globals.
    SetGlobal, // operand: u16 name_index

    // ── Upvalues (closures) ────────────────────────────────────
    /// Push captured upvalue at index.
    GetUpvalue, // operand: u8 index

    // ── Function calls ─────────────────────────────────────────
    /// Call function on stack with `u8` args.
    Call, // operand: u8 argc
    /// Tail-call: reuse current frame.
    TailCall, // operand: u8 argc
    /// Return TOS to caller.
    Return,
    /// Call a builtin directly by name.
    CallBuiltin, // operands: u16 name_index, u8 argc

    // ── Closures ───────────────────────────────────────────────
    /// Create a closure: `u16` function index, `u8` upvalue count,
    /// then N × (u8 is_local, u8 index) upvalue descriptors.
    MakeClosure, // operands: u16 func_index, u8 upvalue_count, then descriptors

    // ── Data constructors ──────────────────────────────────────
    /// Create a tuple from `u8` values.
    MakeTuple, // operand: u8 count
    /// Create a list from `u16` values.
    MakeList, // operand: u16 count
    /// Create a map from `u16` key-value pairs.
    MakeMap, // operand: u16 pair_count
    /// Create a set from `u16` values.
    MakeSet, // operand: u16 count
    /// Create a record: `u16` type name, `u8` field count,
    /// then `u8 field_count` × `u16 field_name_index`.
    MakeRecord, // operands: u16 type_name_index, u8 field_count, then field names
    /// Create a variant value.
    MakeVariant, // operands: u16 name_index, u8 field_count
    /// Functional record update.
    RecordUpdate, // operand: u8 field_count, then field_count × u16 field_name_index
    /// Create a range list from two ints on the stack.
    MakeRange,

    // ── Field access ───────────────────────────────────────────
    /// Access a field by name from TOS.
    GetField, // operand: u16 name_index
    /// Access a tuple element by index.
    GetIndex, // operand: u8 index

    // ── Control flow ───────────────────────────────────────────
    /// Jump forward by `u16` offset.
    Jump, // operand: u16 offset
    /// Jump backward by `u16` offset.
    JumpBack, // operand: u16 offset
    /// Pop TOS; jump forward if falsy.
    JumpIfFalse, // operand: u16 offset
    /// Pop TOS; jump forward if truthy.
    JumpIfTrue, // operand: u16 offset
    /// Discard TOS.
    Pop,
    /// Discard `u8` values.
    PopN, // operand: u8 count
    /// Duplicate TOS.
    Dup,

    // ── Pattern matching ───────────────────────────────────────
    /// Test if TOS variant has tag `constants[u16]`. Peek, push bool.
    TestTag, // operand: u16 name_index
    /// Test if TOS equals `constants[u16]`. Peek, push bool.
    TestEqual, // operand: u16 const_index
    /// Test if TOS tuple has length `u8`. Peek, push bool.
    TestTupleLen, // operand: u8 len
    /// Test if TOS list has length >= `u8`. Peek, push bool.
    TestListMin, // operand: u8 min_len
    /// Test if TOS list has length == `u8`. Peek, push bool.
    TestListExact, // operand: u8 len
    /// Test if TOS int is in range [lo, hi]. Peek, push bool.
    TestIntRange, // operands: inline via constants
    /// Test if TOS float is in range. Peek, push bool.
    TestFloatRange, // operands: inline via constants
    /// Test if TOS is a specific bool value. Peek, push bool.
    TestBool, // operand: u8 (0=false, 1=true)
    /// Extract tuple element at `u8` index. Peek tuple, push element.
    DestructTuple, // operand: u8 index
    /// Extract variant field at `u8` index. Peek variant, push field.
    DestructVariant, // operand: u8 index
    /// Extract list element at `u8` index. Peek list, push element.
    DestructList, // operand: u8 index
    /// Extract list tail from `u8` index. Peek list, push rest.
    DestructListRest, // operand: u8 start
    /// Extract named record field. Peek record, push value.
    DestructRecordField, // operand: u16 name_index
    /// Test if TOS is a record with given type name. Peek, push bool.
    TestRecordTag, // operand: u16 name_index
    /// Test if TOS map contains key. Peek, push bool.
    TestMapHasKey, // operand: u16 const_index (string key)
    /// Extract map value by key. Peek map, push value.
    DestructMapValue, // operand: u16 const_index (string key)

    // ── Loop ───────────────────────────────────────────────────
    /// Store `u8` values from stack into loop-binding locals.
    LoopSetup, // operand: u8 binding_count
    /// Update loop bindings and jump back.
    Recur, // operand: u8 arg_count

    // ── Error handling ─────────────────────────────────────────
    /// Unwrap Ok/Some or early-return Err/None.
    QuestionMark,
    /// Panic with message string on TOS.
    Panic,

    /// Runtime method dispatch: pop receiver, look up "TypeName.method" global, call.
    /// operands: u16 method_name_index, u8 argc (including receiver)
    CallMethod,

    // ── Concurrency ────────────────────────────────────────────
    ChanNew,
    ChanSend,
    ChanRecv,
    ChanClose,
    ChanTrySend,
    ChanTryRecv,
    ChanSelect,
    TaskSpawn,
    TaskJoin,
    TaskCancel,
    /// Cooperative yield point.
    Yield,
}

impl Op {
    /// Convert a raw byte back to an Op.
    ///
    /// Returns `None` if the byte does not correspond to a valid opcode.
    pub fn from_byte(byte: u8) -> Option<Op> {
        match byte {
            b if b == Op::Constant as u8 => Some(Op::Constant),
            b if b == Op::Unit as u8 => Some(Op::Unit),
            b if b == Op::True as u8 => Some(Op::True),
            b if b == Op::False as u8 => Some(Op::False),
            b if b == Op::Add as u8 => Some(Op::Add),
            b if b == Op::Sub as u8 => Some(Op::Sub),
            b if b == Op::Mul as u8 => Some(Op::Mul),
            b if b == Op::Div as u8 => Some(Op::Div),
            b if b == Op::Mod as u8 => Some(Op::Mod),
            b if b == Op::Eq as u8 => Some(Op::Eq),
            b if b == Op::Neq as u8 => Some(Op::Neq),
            b if b == Op::Lt as u8 => Some(Op::Lt),
            b if b == Op::Gt as u8 => Some(Op::Gt),
            b if b == Op::Leq as u8 => Some(Op::Leq),
            b if b == Op::Geq as u8 => Some(Op::Geq),
            b if b == Op::Negate as u8 => Some(Op::Negate),
            b if b == Op::Not as u8 => Some(Op::Not),
            b if b == Op::And as u8 => Some(Op::And),
            b if b == Op::Or as u8 => Some(Op::Or),
            b if b == Op::StringConcat as u8 => Some(Op::StringConcat),
            b if b == Op::DisplayValue as u8 => Some(Op::DisplayValue),
            b if b == Op::GetLocal as u8 => Some(Op::GetLocal),
            b if b == Op::SetLocal as u8 => Some(Op::SetLocal),
            b if b == Op::GetGlobal as u8 => Some(Op::GetGlobal),
            b if b == Op::SetGlobal as u8 => Some(Op::SetGlobal),
            b if b == Op::GetUpvalue as u8 => Some(Op::GetUpvalue),
            b if b == Op::Call as u8 => Some(Op::Call),
            b if b == Op::TailCall as u8 => Some(Op::TailCall),
            b if b == Op::Return as u8 => Some(Op::Return),
            b if b == Op::CallBuiltin as u8 => Some(Op::CallBuiltin),
            b if b == Op::MakeClosure as u8 => Some(Op::MakeClosure),
            b if b == Op::MakeTuple as u8 => Some(Op::MakeTuple),
            b if b == Op::MakeList as u8 => Some(Op::MakeList),
            b if b == Op::MakeMap as u8 => Some(Op::MakeMap),
            b if b == Op::MakeSet as u8 => Some(Op::MakeSet),
            b if b == Op::MakeRecord as u8 => Some(Op::MakeRecord),
            b if b == Op::MakeVariant as u8 => Some(Op::MakeVariant),
            b if b == Op::RecordUpdate as u8 => Some(Op::RecordUpdate),
            b if b == Op::MakeRange as u8 => Some(Op::MakeRange),
            b if b == Op::GetField as u8 => Some(Op::GetField),
            b if b == Op::GetIndex as u8 => Some(Op::GetIndex),
            b if b == Op::Jump as u8 => Some(Op::Jump),
            b if b == Op::JumpBack as u8 => Some(Op::JumpBack),
            b if b == Op::JumpIfFalse as u8 => Some(Op::JumpIfFalse),
            b if b == Op::JumpIfTrue as u8 => Some(Op::JumpIfTrue),
            b if b == Op::Pop as u8 => Some(Op::Pop),
            b if b == Op::PopN as u8 => Some(Op::PopN),
            b if b == Op::Dup as u8 => Some(Op::Dup),
            b if b == Op::TestTag as u8 => Some(Op::TestTag),
            b if b == Op::TestEqual as u8 => Some(Op::TestEqual),
            b if b == Op::TestTupleLen as u8 => Some(Op::TestTupleLen),
            b if b == Op::TestListMin as u8 => Some(Op::TestListMin),
            b if b == Op::TestListExact as u8 => Some(Op::TestListExact),
            b if b == Op::TestIntRange as u8 => Some(Op::TestIntRange),
            b if b == Op::TestFloatRange as u8 => Some(Op::TestFloatRange),
            b if b == Op::TestBool as u8 => Some(Op::TestBool),
            b if b == Op::DestructTuple as u8 => Some(Op::DestructTuple),
            b if b == Op::DestructVariant as u8 => Some(Op::DestructVariant),
            b if b == Op::DestructList as u8 => Some(Op::DestructList),
            b if b == Op::DestructListRest as u8 => Some(Op::DestructListRest),
            b if b == Op::DestructRecordField as u8 => Some(Op::DestructRecordField),
            b if b == Op::TestRecordTag as u8 => Some(Op::TestRecordTag),
            b if b == Op::TestMapHasKey as u8 => Some(Op::TestMapHasKey),
            b if b == Op::DestructMapValue as u8 => Some(Op::DestructMapValue),
            b if b == Op::LoopSetup as u8 => Some(Op::LoopSetup),
            b if b == Op::Recur as u8 => Some(Op::Recur),
            b if b == Op::QuestionMark as u8 => Some(Op::QuestionMark),
            b if b == Op::Panic as u8 => Some(Op::Panic),
            b if b == Op::CallMethod as u8 => Some(Op::CallMethod),
            b if b == Op::ChanNew as u8 => Some(Op::ChanNew),
            b if b == Op::ChanSend as u8 => Some(Op::ChanSend),
            b if b == Op::ChanRecv as u8 => Some(Op::ChanRecv),
            b if b == Op::ChanClose as u8 => Some(Op::ChanClose),
            b if b == Op::ChanTrySend as u8 => Some(Op::ChanTrySend),
            b if b == Op::ChanTryRecv as u8 => Some(Op::ChanTryRecv),
            b if b == Op::ChanSelect as u8 => Some(Op::ChanSelect),
            b if b == Op::TaskSpawn as u8 => Some(Op::TaskSpawn),
            b if b == Op::TaskJoin as u8 => Some(Op::TaskJoin),
            b if b == Op::TaskCancel as u8 => Some(Op::TaskCancel),
            b if b == Op::Yield as u8 => Some(Op::Yield),
            _ => None,
        }
    }
}

// ── Chunk ──────────────────────────────────────────────────────────

/// A chunk of bytecode with its constant pool and source mappings.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Raw bytecode bytes.
    pub code: Vec<u8>,
    /// Constant pool (int, float, string literals and function objects).
    pub constants: Vec<Value>,
    /// Source spans for error reporting, run-length encoded: (bytecode_offset, span).
    pub spans: Vec<(usize, Span)>,
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}

impl Chunk {
    pub fn new() -> Self {
        Chunk {
            code: Vec::new(),
            constants: Vec::new(),
            spans: Vec::new(),
        }
    }

    /// Emit a single byte, returning its offset.
    pub fn emit(&mut self, byte: u8, span: Span) -> usize {
        let offset = self.code.len();
        self.code.push(byte);
        // Only record span if it differs from the last recorded span.
        if self.spans.last().is_none_or(|(_, s)| *s != span) {
            self.spans.push((offset, span));
        }
        offset
    }

    /// Emit an opcode.
    pub fn emit_op(&mut self, op: Op, span: Span) -> usize {
        self.emit(op as u8, span)
    }

    /// Emit a u16 operand (little-endian).
    pub fn emit_u16(&mut self, value: u16, span: Span) {
        self.emit(value as u8, span);
        self.emit((value >> 8) as u8, span);
    }

    /// Emit a u8 operand.
    pub fn emit_u8(&mut self, value: u8, span: Span) {
        self.emit(value, span);
    }

    /// Read a u16 at the given offset (little-endian).
    pub fn read_u16(&self, offset: usize) -> u16 {
        self.code[offset] as u16 | ((self.code[offset + 1] as u16) << 8)
    }

    /// Add a constant to the pool, returning its index.
    /// Deduplicates integers, booleans, and strings.
    pub fn add_constant(&mut self, value: Value) -> u16 {
        // Deduplicate simple constants.
        for (i, existing) in self.constants.iter().enumerate() {
            match (&value, existing) {
                (Value::Int(a), Value::Int(b)) if a == b => return i as u16,
                (Value::Bool(a), Value::Bool(b)) if a == b => return i as u16,
                (Value::String(a), Value::String(b)) if a == b => return i as u16,
                (Value::Float(a), Value::Float(b)) if a.to_bits() == b.to_bits() => {
                    return i as u16;
                }
                _ => {}
            }
        }
        let index = self.constants.len();
        assert!(index <= u16::MAX as usize, "constant pool overflow");
        self.constants.push(value);
        index as u16
    }

    /// Emit a placeholder jump and return the offset to patch later.
    pub fn emit_jump(&mut self, op: Op, span: Span) -> usize {
        self.emit_op(op, span);
        let patch_offset = self.code.len();
        self.emit_u16(0xFFFF, span); // placeholder
        patch_offset
    }

    /// Patch a previously emitted jump's u16 operand to point to the current offset.
    pub fn patch_jump(&mut self, patch_offset: usize) {
        let target = self.code.len();
        let jump_base = patch_offset + 2; // after the u16 operand
        let offset = target - jump_base;
        assert!(offset <= u16::MAX as usize, "jump offset overflow");
        self.code[patch_offset] = offset as u8;
        self.code[patch_offset + 1] = (offset >> 8) as u8;
    }

    /// Get the source span for a bytecode offset.
    pub fn span_at(&self, offset: usize) -> Span {
        // Binary search for the last span entry <= offset.
        let mut result = Span::new(0, 0);
        for &(off, span) in &self.spans {
            if off <= offset {
                result = span;
            } else {
                break;
            }
        }
        result
    }

    /// Current length of bytecode.
    pub fn len(&self) -> usize {
        self.code.len()
    }

    /// Whether the chunk contains no bytecode.
    pub fn is_empty(&self) -> bool {
        self.code.is_empty()
    }
}

// ── Function ───────────────────────────────────────────────────────

/// A compiled function (or the top-level script).
#[derive(Debug, Clone)]
pub struct Function {
    /// Function name (for debugging and stack traces).
    pub name: String,
    /// Number of parameters.
    pub arity: u8,
    /// Number of upvalues this function captures.
    pub upvalue_count: u8,
    /// The compiled bytecode.
    pub chunk: Chunk,
}

impl Function {
    pub fn new(name: String, arity: u8) -> Self {
        Function {
            name,
            arity,
            upvalue_count: 0,
            chunk: Chunk::new(),
        }
    }
}

// ── VmClosure ──────────────────────────────────────────────────────

/// Build a tiny script that calls a named global function with no arguments
/// and returns the result.  Useful for the test runner and REPL.
pub fn call_global_script(name: &str) -> Function {
    let span = Span::new(0, 0);
    let mut func = Function::new(format!("<call:{name}>"), 0);
    let idx = func.chunk.add_constant(Value::String(name.into()));
    func.chunk.emit_op(Op::GetGlobal, span);
    func.chunk.emit_u16(idx, span);
    func.chunk.emit_op(Op::Call, span);
    func.chunk.emit_u8(0, span);
    func.chunk.emit_op(Op::Return, span);
    func
}

/// A runtime closure: a compiled function + captured upvalues.
///
/// Since silt is fully immutable, upvalues are simple value copies
/// captured at closure creation time. No open/closed distinction needed.
#[derive(Debug, Clone)]
pub struct VmClosure {
    pub function: Arc<Function>,
    pub upvalues: Vec<Value>,
}

// ── Upvalue descriptor (compile-time) ──────────────────────────────

/// Describes how to capture an upvalue when creating a closure.
#[derive(Debug, Clone, Copy)]
pub struct UpvalueDesc {
    /// If true, captures a local from the immediately enclosing scope.
    /// If false, captures an upvalue from the enclosing closure (transitive).
    pub is_local: bool,
    /// The slot index (local slot or parent upvalue index).
    pub index: u8,
}
