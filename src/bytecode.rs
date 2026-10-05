//! Bytecode definitions for Silt's stack-based VM.
//!
//! A `Function` is the compilation unit — it holds a `Chunk` of bytecode,
//! a constant pool, and source span mappings for error reporting.

use std::collections::HashMap;
use std::sync::Arc;

use crate::defs::{DefId, TraitId, TypeId};
use crate::source::Span;
use crate::typeinfo::TypeInfo;
use crate::value::Value;

/// A dedup key for simple constant types.  Using a dedicated enum avoids
/// relying on `Value`'s `Hash`/`Eq` and keeps the dedup scope explicit.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum ConstantKey {
    Int(i64),
    Bool(bool),
    String(String),
    Float(u64), // f64::to_bits()
    /// A variant constructor (a pattern's variant test): its type and
    /// ordinal.
    Variant(TypeId, u16),
    /// A variant without fields.
    Nullary(TypeId, u16),
    /// A type descriptor (a record literal's or pattern's type).
    Type(TypeId),
    /// A builtin function used as a value (`println`), by its name.
    Builtin(String),
    /// A primitive type's descriptor (`Int` as a value), by its name.
    Primitive(String),
}

// ── Global slots ───────────────────────────────────────────────────

/// The global slots of a program. Every top-level function, `let` and
/// host function of its modules has one, and so has every method of an
/// impl, by the impl's trait, the type it is for and the method's name.
/// A REPL session keeps one `Globals` for all its entries: a definition
/// an entry makes again is a new definition with a slot of its own.
#[derive(Debug, Clone, Default)]
pub struct Globals {
    /// The name of each slot's definition, as disassembly and errors
    /// show it.
    names: Vec<String>,
    defs: HashMap<DefId, u16>,
    methods: HashMap<(TraitId, TypeId), HashMap<String, u16>>,
    /// The methods of each type whatever their trait, for a call whose
    /// trait is not known where it is compiled (a call in a polymorphic
    /// function with no bound for the receiver), with the trait of each.
    /// `None` where two traits provide the method for the type: such a
    /// call is ambiguous.
    by_type: HashMap<TypeId, HashMap<String, Option<(u16, TraitId)>>>,
    /// The traits a `CallMethod` names, by the index its operand holds,
    /// each with its name.
    traits: Vec<(TraitId, String)>,
}

/// The `CallMethod` trait operand of a call whose trait is not known
/// where it is compiled.
pub const NO_TRAIT: u16 = u16::MAX;

impl Globals {
    /// The number of slots.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The slot of the definition `def`.
    pub fn def(&self, def: DefId) -> Option<u16> {
        self.defs.get(&def).copied()
    }

    /// The slot of the method `method` of the impl of the trait `t` (any
    /// trait's, for `None`) for the type `ty`.
    pub fn method(&self, t: Option<TraitId>, ty: TypeId, method: &str) -> Option<u16> {
        match t {
            Some(t) => self.methods.get(&(t, ty))?.get(method).copied(),
            None => self
                .by_type
                .get(&ty)?
                .get(method)
                .copied()
                .flatten()
                .map(|(slot, _)| slot),
        }
    }

    /// Whether a call of `method` on a value of the type `ty` that names
    /// no trait is ambiguous: two traits' impls provide the method for
    /// the type, or one trait's impl and, for a type whose builtin trait
    /// methods are native (`native`: `Int`, `List`, ...), the builtin
    /// trait the method is of (`display` of Display).
    pub fn ambiguous(&self, ty: TypeId, method: &str, native: bool) -> bool {
        match self.by_type.get(&ty).and_then(|m| m.get(method)) {
            Some(None) => true,
            Some(Some((_, t))) => {
                native && crate::defs::builtin_trait_of_method(method).is_some_and(|b| b != *t)
            }
            None => false,
        }
    }

    /// [`Globals::method`] for a `CallMethod` trait operand.
    pub fn call_method(&self, trait_index: u16, ty: TypeId, method: &str) -> Option<u16> {
        let t = self.traits.get(trait_index as usize).map(|(t, _)| *t);
        self.method(t, ty, method)
    }

    /// The name of slot `slot`.
    pub fn name(&self, slot: u16) -> &str {
        self.names.get(slot as usize).map_or("?", String::as_str)
    }

    /// The name of the trait a `CallMethod` operand names.
    pub fn trait_name(&self, trait_index: u16) -> Option<&str> {
        self.traits
            .get(trait_index as usize)
            .map(|(_, n)| n.as_str())
    }

    /// The `CallMethod` operand of the trait `t`, named `name`; `None`
    /// when 65,535 traits are named already.
    pub fn trait_index(&mut self, t: TraitId, name: String) -> Option<u16> {
        if let Some(k) = self.traits.iter().position(|(known, _)| *known == t) {
            return Some(k as u16);
        }
        let k = u16::try_from(self.traits.len())
            .ok()
            .filter(|k| *k != NO_TRAIT)?;
        self.traits.push((t, name));
        Some(k)
    }

    /// A new slot named `name`; `None` when all 65,536 slots are taken.
    fn add(&mut self, name: String) -> Option<u16> {
        let slot = u16::try_from(self.names.len()).ok()?;
        self.names.push(name);
        Some(slot)
    }

    /// The slot of the definition `def`, named `name`: a new one the
    /// first time.
    pub fn add_def(&mut self, def: DefId, name: String) -> Option<u16> {
        if let Some(slot) = self.def(def) {
            return Some(slot);
        }
        let slot = self.add(name)?;
        self.defs.insert(def, slot);
        Some(slot)
    }

    /// The slot of the method `method` of the impl of the trait `t` for
    /// the type `ty`, named `name`: a new one the first time. A later
    /// impl of the trait for the type (a REPL entry's) takes the slot
    /// over.
    pub fn add_method(
        &mut self,
        t: TraitId,
        ty: TypeId,
        method: &str,
        name: String,
    ) -> Option<u16> {
        if let Some(slot) = self.method(Some(t), ty, method) {
            return Some(slot);
        }
        let slot = self.add(name)?;
        self.methods
            .entry((t, ty))
            .or_default()
            .insert(method.to_string(), slot);
        self.by_type
            .entry(ty)
            .or_default()
            .entry(method.to_string())
            .and_modify(|known| *known = None)
            .or_insert(Some((slot, t)));
        Some(slot)
    }
}

// ── Record types ───────────────────────────────────────────────────

/// Whether a record of the type `ty` satisfies a check for the nominal
/// record type `expected`. A record built from an anonymous record
/// literal (`{x: 1}`) or bound by a record rest pattern satisfies every
/// check: the typechecker lets such a value flow wherever a nominal
/// record of the same shape is expected, and has already proved the
/// shapes agree. This is the one rule every run-time record-type check
/// uses (pattern tests, builtins that accept a `Date`/`Response`/...
/// record).
pub fn record_type_matches(ty: &TypeInfo, expected: TypeId) -> bool {
    ty.id == expected || ty.is_anon()
}

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
    /// Push the value of global slot `u16` (see [`Globals`]).
    GetGlobal, // operand: u16 slot
    /// Store TOS into global slot `u16`. Does NOT pop.
    SetGlobal, // operand: u16 slot

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
    /// Create a record: `u16` index of the type's descriptor constant,
    /// `u8` field count, then `u8 field_count` × `u16 field_name_index`.
    MakeRecord, // operands: u16 type_index, u8 field_count, then field names
    /// Functional record update.
    RecordUpdate, // operand: u8 field_count, then field_count × u16 field_name_index
    //
    // Round-85 follow-up: removed `RecordUpdateAnon` (the sibling opcode
    // round 83 added to normalize the runtime `type_name` to `"<anon>"`
    // for spread expressions whose typed result was `Type::AnonRecord`).
    // Round 84's `Value::PartialEq` `<anon>` wildcard plus round 85's
    // mirrored `Ord`/`Hash` wildcards close the equality, hashing, and
    // ordering surfaces uniformly; the runtime tag-rebrand is therefore
    // strictly redundant for correctness, and removing it gives one
    // canonical opcode for record updates ("one way to do things"). The
    // displayed name in `println(r)` for a spread-of-nominal now reads
    // the nominal base's name rather than `<anon>` — strictly more
    // informative for debugging.
    /// Create a lazy range (inclusive) from two ints on the stack.
    MakeRange,
    /// Concatenate two lists/ranges on the stack into a single list.
    ListConcat,

    // ── Field access ───────────────────────────────────────────
    /// Access a field by name from TOS.
    GetField, // operand: u16 name_index

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
    /// Duplicate TOS.
    Dup,

    // ── Pattern matching ───────────────────────────────────────
    /// Test if TOS is the variant whose constructor is `constants[u16]`.
    /// Peek, push bool.
    TestTag, // operand: u16 const_index
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
    /// Construct a new record from TOS by removing the listed field
    /// names. The record on TOS is consumed (popped) and a new
    /// anonymous `Value::Record` of the fields minus the excluded ones is
    /// pushed. Used by row-polymorphic anon-record patterns to bind
    /// the `...rest` portion. Layout: u8 count, then count u16 name
    /// indices into the constant pool (string).
    DestructRecordRest, // operand: u8 count, count*u16 name indices
    /// Test if TOS is a record of the type whose descriptor is
    /// `constants[u16]`, or an anonymous record (see
    /// [`record_type_matches`]). Peek, push bool.
    TestRecordTag, // operand: u16 const_index
    /// Test if TOS map contains key. Peek, push bool.
    TestMapHasKey, // operand: u16 const_index (string key)
    /// Extract map value by key. Peek map, push value.
    DestructMapValue, // operand: u16 const_index (string key)

    // ── Loop ───────────────────────────────────────────────────
    /// Update loop bindings and jump back.
    Recur, // operands: u8 arg_count, u16 first_slot

    // ── Error handling ─────────────────────────────────────────
    /// Unwrap Ok/Some or early-return Err/None.
    QuestionMark,
    /// Panic with message string on TOS.
    Panic,

    /// Runtime method dispatch: the method named by the constant, of the
    /// impl of the trait the third operand names (see
    /// [`Globals::call_method`]; [`NO_TRAIT`] for any trait's) for the
    /// receiver's type, else a builtin trait method or a record field
    /// holding a function; call it.
    /// operands: u16 method_name_index, u8 argc (including receiver),
    /// u16 trait
    CallMethod,

    /// Move TOS down to `stack[frame_base + u16]` and drop every value that
    /// was above that slot: pop TOS, cut the frame back to `u16` values,
    /// push the popped value. The compiler emits it where a scope ends with
    /// its locals still under the result, and where a failed pattern test
    /// lands, so that the frame again holds exactly the values the compiler
    /// has accounted for.
    Slide, // operand: u16 slot
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
            b if b == Op::RecordUpdate as u8 => Some(Op::RecordUpdate),
            b if b == Op::MakeRange as u8 => Some(Op::MakeRange),
            b if b == Op::ListConcat as u8 => Some(Op::ListConcat),
            b if b == Op::GetField as u8 => Some(Op::GetField),
            b if b == Op::Jump as u8 => Some(Op::Jump),
            b if b == Op::JumpBack as u8 => Some(Op::JumpBack),
            b if b == Op::JumpIfFalse as u8 => Some(Op::JumpIfFalse),
            b if b == Op::JumpIfTrue as u8 => Some(Op::JumpIfTrue),
            b if b == Op::Pop as u8 => Some(Op::Pop),
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
            b if b == Op::DestructRecordRest as u8 => Some(Op::DestructRecordRest),
            b if b == Op::TestRecordTag as u8 => Some(Op::TestRecordTag),
            b if b == Op::TestMapHasKey as u8 => Some(Op::TestMapHasKey),
            b if b == Op::DestructMapValue as u8 => Some(Op::DestructMapValue),
            b if b == Op::Recur as u8 => Some(Op::Recur),
            b if b == Op::QuestionMark as u8 => Some(Op::QuestionMark),
            b if b == Op::Panic as u8 => Some(Op::Panic),
            b if b == Op::CallMethod as u8 => Some(Op::CallMethod),
            b if b == Op::Slide as u8 => Some(Op::Slide),
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
    /// O(1) lookup for deduplicating simple constants.
    constant_dedup: HashMap<ConstantKey, u16>,
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
            constant_dedup: HashMap::new(),
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

    /// Emit an opcode immediately followed by a u16 operand sharing the same span.
    pub fn emit_op_u16(&mut self, op: Op, idx: u16, span: Span) -> usize {
        let offset = self.emit_op(op, span);
        self.emit_u16(idx, span);
        offset
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
    /// Deduplicates integers, booleans, strings, and floats via O(1) HashMap lookup.
    /// Returns an error if the constant pool exceeds the u16 limit.
    pub fn add_constant(&mut self, value: Value) -> Result<u16, String> {
        let key = match &value {
            Value::Int(n) => Some(ConstantKey::Int(*n)),
            Value::Bool(b) => Some(ConstantKey::Bool(*b)),
            Value::String(s) => Some(ConstantKey::String(s.clone())),
            Value::Float(f) => Some(ConstantKey::Float(f.to_bits())),
            Value::VariantConstructor(tag) => {
                Some(ConstantKey::Variant(tag.type_id(), tag.ordinal()))
            }
            Value::Variant(tag, fields) if fields.is_empty() => {
                Some(ConstantKey::Nullary(tag.type_id(), tag.ordinal()))
            }
            Value::TypeDescriptor(ty) => Some(ConstantKey::Type(ty.id)),
            Value::BuiltinFn(name) => Some(ConstantKey::Builtin(name.clone())),
            Value::PrimitiveDescriptor(name) => Some(ConstantKey::Primitive(name.clone())),
            _ => None,
        };

        if let Some(k) = key {
            if let Some(&idx) = self.constant_dedup.get(&k) {
                return Ok(idx);
            }
            let index = self.constants.len();
            if index > u16::MAX as usize {
                return Err("constant pool overflow: too many constants in function".into());
            }
            self.constants.push(value);
            let idx = index as u16;
            self.constant_dedup.insert(k, idx);
            return Ok(idx);
        }

        let index = self.constants.len();
        if index > u16::MAX as usize {
            return Err("constant pool overflow: too many constants in function".into());
        }
        self.constants.push(value);
        Ok(index as u16)
    }

    /// Emit a placeholder jump and return the offset to patch later.
    pub fn emit_jump(&mut self, op: Op, span: Span) -> usize {
        self.emit_op(op, span);
        let patch_offset = self.code.len();
        self.emit_u16(0xFFFF, span); // placeholder
        patch_offset
    }

    /// Patch a previously emitted jump's u16 operand to point to the current offset.
    /// Returns an error if the jump offset exceeds the u16 limit.
    pub fn patch_jump(&mut self, patch_offset: usize) -> Result<(), String> {
        self.patch_jump_to(patch_offset, self.code.len())
    }

    /// Patch a previously emitted jump's u16 operand to point to a specific target offset.
    /// Returns an error if the jump offset exceeds the u16 limit.
    pub fn patch_jump_to(&mut self, patch_offset: usize, target: usize) -> Result<(), String> {
        let jump_base = patch_offset + 2; // after the u16 operand
        let offset = target - jump_base;
        if offset > u16::MAX as usize {
            return Err("jump offset overflow: function body too large".into());
        }
        self.code[patch_offset] = offset as u8;
        self.code[patch_offset + 1] = (offset >> 8) as u8;
        Ok(())
    }

    /// Get the source span for a bytecode offset: `Span::BUILTIN` before
    /// the first recorded span.
    pub fn span_at(&self, offset: usize) -> Span {
        // Linear scan for the last span entry <= offset. The spans table is
        // appended in strictly ascending offset order during emission, so a
        // forward scan that breaks on the first entry past `offset` is
        // correct; it's deliberately linear to keep the common (near-end)
        // case fast and to avoid binary-search bookkeeping overhead.
        let mut result = Span::BUILTIN;
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

/// Build a tiny script that calls the function in global slot `slot`,
/// `name`, with no arguments and returns the result: the test runner
/// calls each test so. The call is silt's own, so its code has
/// `Span::BUILTIN`.
pub(crate) fn call_global_script(slot: u16, name: &str) -> Function {
    let span = Span::BUILTIN;
    let mut func = Function::new(format!("<call:{name}>"), 0);
    func.chunk.emit_op(Op::GetGlobal, span);
    func.chunk.emit_u16(slot, span);
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
