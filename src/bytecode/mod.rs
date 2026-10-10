//! Bytecode definitions for Silt's stack-based VM.
//!
//! A `Function` is the compilation unit — it holds a `Chunk` of bytecode,
//! a constant pool, and source span mappings for error reporting.
//!
//! The instructions are one table ([`ops`]). The compiler writes them
//! through the [`Emitter`] and nothing else writes them; every function
//! the emitter gives has passed the verifier ([`verify`](mod@verify)).

use std::collections::HashMap;
use std::sync::Arc;

use crate::builtins::registry::BuiltinId;
use crate::defs::{DefId, TraitId, TypeId};
use crate::source::Span;
use crate::typeinfo::TypeInfo;
use crate::value::Value;

pub mod emit;
pub mod ops;
pub mod verify;

pub use emit::Emitter;
pub use ops::{Asm, Instr, Op, Operands, decode};
pub use verify::{VerifyError, verify};

/// A dedup key for simple constant types.  Using a dedicated enum avoids
/// relying on `Value`'s `Hash`/`Eq` and keeps the dedup scope explicit.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum ConstantKey {
    Int(i64),
    Bool(bool),
    String(Arc<str>),
    Float(u64), // f64::to_bits()
    /// A variant constructor (a pattern's variant test): its type and
    /// ordinal.
    Variant(TypeId, u16),
    /// A variant without fields.
    Nullary(TypeId, u16),
    /// A type descriptor (a record literal's or pattern's type).
    Type(TypeId),
    /// The descriptor of an anonymous record type (an anonymous record
    /// literal's): the types of all such share one id, and there is
    /// one description for each set of field names
    /// ([`crate::typeinfo::anon_record_type`]), which the constant
    /// holds; this is where it is.
    AnonType(usize),
    /// A builtin function used as a value (`println`), by its row's
    /// id.
    Builtin(BuiltinId),
    /// A primitive type's descriptor (`Int` as a value), by its name.
    Primitive(&'static str),
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
    /// The default methods of each trait: the body the trait writes,
    /// compiled once. An impl that leaves the method out has this slot
    /// as its method's.
    defaults: HashMap<(TraitId, String), u16>,
    /// The slot of `display` of each type a program wrote a `Display`
    /// impl for: what showing a value of the type calls.
    shown: HashMap<TypeId, u16>,
    /// One bit for each type in `shown`, by the low bits of its id: a
    /// type whose bit is not set has no written `display`, which is
    /// what showing a value asks of every record and variant in it.
    shown_bits: u64,
    /// The traits a `CallMethod` names, by the index its operand holds,
    /// each with its name.
    traits: Vec<(TraitId, String)>,
}

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

    /// The slot of the method `method` of the impl of the trait `t` for
    /// the type `ty`.
    pub fn method(&self, t: TraitId, ty: TypeId, method: &str) -> Option<u16> {
        self.methods.get(&(t, ty))?.get(method).copied()
    }

    /// The number of traits a `CallMethod` operand can name.
    pub fn trait_count(&self) -> usize {
        self.traits.len()
    }

    /// [`Globals::method`] for a `CallMethod` trait operand.
    pub fn call_method(&self, trait_index: u16, ty: TypeId, method: &str) -> Option<u16> {
        let (t, _) = self.traits.get(trait_index as usize)?;
        self.method(*t, ty, method)
    }

    /// The name of slot `slot`.
    pub fn name(&self, slot: u16) -> &str {
        self.names.get(slot as usize).map_or("?", String::as_str)
    }

    /// The trait a `CallMethod` operand names.
    pub fn trait_at(&self, trait_index: u16) -> Option<TraitId> {
        self.traits.get(trait_index as usize).map(|(t, _)| *t)
    }

    /// The slot of the `display` a program wrote for the type `ty`.
    pub fn shown(&self, ty: TypeId) -> Option<u16> {
        if self.shown_bits & Self::shown_bit(ty) == 0 {
            return None;
        }
        self.shown.get(&ty).copied()
    }

    fn shown_bit(ty: TypeId) -> u64 {
        1 << (ty.0.0 & 63)
    }

    /// Whether the program wrote a `Display` impl for any type.
    pub fn any_shown(&self) -> bool {
        !self.shown.is_empty()
    }

    /// The name of the trait a `CallMethod` operand names.
    pub fn trait_name(&self, trait_index: u16) -> Option<&str> {
        self.traits
            .get(trait_index as usize)
            .map(|(_, n)| n.as_str())
    }

    /// The `CallMethod` operand of the trait `t`, named `name`; `None`
    /// when 65,536 traits are named already.
    pub fn trait_index(&mut self, t: TraitId, name: String) -> Option<u16> {
        if let Some(k) = self.traits.iter().position(|(known, _)| *known == t) {
            return u16::try_from(k).ok();
        }
        let k = u16::try_from(self.traits.len()).ok()?;
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

    /// The slot of the default method `method` of the trait `t`.
    pub fn default_method(&self, t: TraitId, method: &str) -> Option<u16> {
        self.defaults.get(&(t, method.to_string())).copied()
    }

    /// The default methods of the trait `t`, each with its slot, by
    /// name: whatever declared the trait (the program, a module it
    /// imports, an earlier REPL entry, the builtins).
    pub fn default_methods(&self, t: TraitId) -> Vec<(String, u16)> {
        let mut methods: Vec<(String, u16)> = self
            .defaults
            .iter()
            .filter(|((of, _), _)| *of == t)
            .map(|((_, method), slot)| (method.clone(), *slot))
            .collect();
        methods.sort();
        methods
    }

    /// The slot of the default method `method` of the trait `t`, named
    /// `name`: a new one the first time.
    pub fn add_default_method(&mut self, t: TraitId, method: &str, name: String) -> Option<u16> {
        if let Some(slot) = self.default_method(t, method) {
            return Some(slot);
        }
        let slot = self.add(name)?;
        self.defaults.insert((t, method.to_string()), slot);
        Some(slot)
    }

    /// The impl of the trait `t` for the type `ty` leaves `method` out:
    /// its method is the trait's default, in `slot`.
    pub fn default_for(&mut self, t: TraitId, ty: TypeId, method: &str, slot: u16) {
        if self.method(t, ty, method).is_some() {
            return;
        }
        self.methods
            .entry((t, ty))
            .or_default()
            .insert(method.to_string(), slot);
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
        if let Some(slot) = self.method(t, ty, method) {
            return Some(slot);
        }
        let slot = self.add(name)?;
        self.methods
            .entry((t, ty))
            .or_default()
            .insert(method.to_string(), slot);
        if method == "display" && crate::defs::builtin_trait_id("Display") == Some(t) {
            self.shown.insert(ty, slot);
            self.shown_bits |= Self::shown_bit(ty);
        }
        Some(slot)
    }
}

// ── Operands the compiler names ────────────────────────────────────

/// A constant of a function, by its index in the function's pool. The
/// emitter hands them out ([`Emitter::constant`]); the decoder reads
/// them back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Const(u16);

impl Const {
    /// The index of the constant in its pool.
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// A place in a function's code that jumps go to. The emitter hands
/// them out ([`Emitter::label`]) and gives each its place
/// ([`Emitter::bind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Label(usize);

/// Describes how to capture an upvalue when creating a closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpvalueDesc {
    /// If true, captures a local from the immediately enclosing scope.
    /// If false, captures an upvalue from the enclosing closure (transitive).
    pub is_local: bool,
    /// The slot index (local slot or parent upvalue index).
    pub index: usize,
}

// ── Chunk ──────────────────────────────────────────────────────────

/// A chunk of bytecode with its constant pool and source mappings. Only
/// the [`Emitter`] writes one.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// The encoded instructions (see [`ops`]).
    code: Vec<u8>,
    /// Constant pool (int, float, string literals and function objects).
    constants: Vec<Value>,
    /// Source spans for error reporting, run-length encoded: (bytecode_offset, span).
    spans: Vec<(usize, Span)>,
    /// O(1) lookup for deduplicating simple constants.
    constant_dedup: HashMap<ConstantKey, u16>,
}

impl Chunk {
    fn new() -> Self {
        Chunk {
            code: Vec::new(),
            constants: Vec::new(),
            spans: Vec::new(),
            constant_dedup: HashMap::new(),
        }
    }

    /// The encoded instructions.
    pub fn code(&self) -> &[u8] {
        &self.code
    }

    /// The constant pool.
    pub fn constants(&self) -> &[Value] {
        &self.constants
    }

    /// The instructions with their offsets, in the order of the code.
    pub fn instrs(&self) -> impl Iterator<Item = (usize, Instr)> + '_ {
        let mut at = 0;
        std::iter::from_fn(move || {
            let (instr, next) = decode(&self.code, at)?;
            let offset = std::mem::replace(&mut at, next);
            Some((offset, instr))
        })
    }

    /// The constant `k`.
    pub fn constant(&self, k: Const) -> &Value {
        &self.constants[k.index()]
    }

    /// The string constant `k`: the operand of an instruction that takes
    /// a string, as the verifier checked.
    pub fn string(&self, k: Const) -> &str {
        match self.constant(k) {
            Value::String(s) => s,
            other => unreachable!("verified code names a string constant, not {other:?}"),
        }
    }

    /// The variant of the constructor constant `k` (a pattern's variant
    /// test).
    pub fn tag(&self, k: Const) -> &crate::typeinfo::Tag {
        match self.constant(k) {
            Value::VariantConstructor(tag) => tag,
            other => unreachable!("verified code names a variant constant, not {other:?}"),
        }
    }

    /// The type of the descriptor constant `k` (a record literal's or
    /// pattern's type).
    pub fn type_info(&self, k: Const) -> &Arc<TypeInfo> {
        match self.constant(k) {
            Value::TypeDescriptor(ty) => ty,
            other => unreachable!("verified code names a type constant, not {other:?}"),
        }
    }

    /// The function constant `k` (the function a `MakeClosure` closes).
    pub fn closure(&self, k: Const) -> &Arc<VmClosure> {
        match self.constant(k) {
            Value::VmClosure(closure) => closure,
            other => unreachable!("verified code names a function constant, not {other:?}"),
        }
    }

    /// Append a byte of the instruction at `span`.
    fn push(&mut self, byte: u8, span: Span) {
        // Only record span if it differs from the last recorded span.
        if self.spans.last().is_none_or(|(_, s)| *s != span) {
            self.spans.push((self.code.len(), span));
        }
        self.code.push(byte);
    }

    /// Add a constant to the pool, returning its index; `None` when the
    /// pool is full (65,536 constants).
    /// Deduplicates integers, booleans, strings, and floats via O(1) HashMap lookup.
    fn add_constant(&mut self, value: Value) -> Option<Const> {
        let key = match &value {
            Value::Int(n) => Some(ConstantKey::Int(*n)),
            Value::Bool(b) => Some(ConstantKey::Bool(*b)),
            Value::String(s) => Some(ConstantKey::String(s.clone())),
            Value::Float(f) => Some(ConstantKey::Float(f.to_bits())),
            Value::VariantConstructor(tag) => {
                Some(ConstantKey::Variant(tag.type_id(), tag.ordinal()))
            }
            Value::Variant(variant) if variant.fields().is_empty() => {
                Some(ConstantKey::Nullary(variant.type_id(), variant.ordinal()))
            }
            Value::TypeDescriptor(ty) if ty.is_anon() => {
                Some(ConstantKey::AnonType(Arc::as_ptr(ty) as usize))
            }
            Value::TypeDescriptor(ty) => Some(ConstantKey::Type(ty.id)),
            Value::BuiltinFn(id) => Some(ConstantKey::Builtin(*id)),
            Value::PrimitiveDescriptor(name) => Some(ConstantKey::Primitive(name)),
            _ => None,
        };
        if let Some(&index) = key.as_ref().and_then(|k| self.constant_dedup.get(k)) {
            return Some(Const(index));
        }
        let index = u16::try_from(self.constants.len()).ok()?;
        self.constants.push(value);
        if let Some(k) = key {
            self.constant_dedup.insert(k, index);
        }
        Some(Const(index))
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

/// A compiled function (or the top-level script). Every `Function` has
/// passed the verifier ([`verify`](mod@verify)): [`Emitter::finish`] is the one way
/// to make one, so the VM runs verified code only.
#[derive(Debug, Clone)]
pub struct Function {
    /// Function name (for debugging and stack traces).
    name: String,
    /// Number of parameters.
    arity: u8,
    /// Number of upvalues this function captures.
    upvalue_count: u8,
    /// The compiled bytecode.
    chunk: Chunk,
}

impl Function {
    /// A function named `name` that takes `arity` arguments and returns
    /// unit: a stand-in where a function value is needed and its code is
    /// not (a test of how closures compare or print).
    pub fn returning_unit(name: String, arity: u8) -> Self {
        let span = Span::BUILTIN;
        let build = || {
            let mut emitter = Emitter::new(name, usize::from(arity), span)?;
            emitter.emit(Asm::Unit, span)?;
            emitter.emit(Asm::Return, span)?;
            emitter.finish(0)
        };
        build().expect("the code of a function that returns unit is well-formed")
    }

    /// The function's name (for debugging and stack traces).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The number of parameters.
    pub fn arity(&self) -> usize {
        usize::from(self.arity)
    }

    /// The number of upvalues the function captures.
    pub fn upvalue_count(&self) -> usize {
        usize::from(self.upvalue_count)
    }

    /// The compiled bytecode.
    pub fn chunk(&self) -> &Chunk {
        &self.chunk
    }

    /// A function made of `code` and `constants` as they are, unverified:
    /// what the verifier's tests hand it.
    #[cfg(test)]
    pub(crate) fn unverified(
        arity: u8,
        upvalue_count: u8,
        code: Vec<u8>,
        constants: Vec<Value>,
    ) -> Self {
        Function {
            name: "<unverified>".into(),
            arity,
            upvalue_count,
            chunk: Chunk {
                code,
                constants,
                spans: Vec::new(),
                constant_dedup: HashMap::new(),
            },
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
    let build = || {
        let mut emitter = Emitter::new(format!("<call:{name}>"), 0, span)?;
        emitter.emit(Asm::GetGlobal { slot }, span)?;
        emitter.emit(Asm::Call { argc: 0 }, span)?;
        emitter.emit(Asm::Return, span)?;
        emitter.finish(0)
    };
    build().expect("the code of a call of a global is well-formed")
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
