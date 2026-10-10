//! The instruction table.
//!
//! `ops!` is invoked once, at the end of this file, with one row per
//! instruction: its name, its operands, and what it does to the frame.
//! Everything that has to agree about an instruction is generated from
//! its row:
//!
//! - [`Op`], the opcode byte, and its name;
//! - [`Asm`], the instruction as the compiler hands it to the
//!   [`Emitter`](super::Emitter), and its encoder, which is the one place
//!   an operand is narrowed to its encoded width (an operand too large
//!   for it is a [`Limit`]);
//! - [`Instr`], the instruction as [`decode`] reads it back, for the VM
//!   loop, the disassembler and the verifier;
//! - the [`Effect`] of the instruction on the frame's height, which the
//!   emitter keeps the height with and the verifier checks it with;
//! - the [`Operand`]s of a decoded instruction with their kinds, which
//!   the verifier checks one by one.
//!
//! # Encoding
//!
//! An instruction is its opcode byte followed by its operands in the
//! order of the row. Multi-byte operands are little-endian.
//!
//! | Kind | Encoded as | In [`Asm`] | In [`Instr`] | The verifier checks |
//! |---|---|---|---|---|
//! | `U8` | `u8` | `usize` | `usize` | |
//! | `U16` | `u16` | `usize` | `usize` | |
//! | `Slot` | `u16` | `usize` | `usize` | below the frame's height |
//! | `Cut` | `u16` | `usize` | `usize` | (through the row's `cut`) |
//! | `Upvalue` | `u8` | `usize` | `usize` | below the function's upvalue count |
//! | `Global` | `u16` | `u16` | `u16` | |
//! | `Trait` | `u16` | `u16` | `u16` | |
//! | `Builtin` | `u16` | [`BuiltinId`] | [`BuiltinId`] | a function row of the builtin registry that is built |
//! | `Const` | `u16` | [`Const`] | [`Const`] | in the pool |
//! | `Str`, `Tag`, `Type`, `Func`, `Int`, `Float` | `u16` | [`Const`] | [`Const`] | in the pool, of that kind |
//! | `Fwd` | `u16` distance forward from the next instruction | [`Label`] | target offset | an instruction starts there |
//! | `Rel` | `i32` distance from the next instruction, forward or back | [`Label`] | target offset | an instruction starts there |
//! | `Strs` | `u8` count, then `u16` each | `&[Const]` | [`Operands<Const>`] | each a string in the pool |
//! | `Captures` | `u8` count, then `u8 is_local, u8 index` each | `&[UpvalueDesc]` | [`Operands<UpvalueDesc>`] | each names a slot below the height or an upvalue of the function |
//!
//! A narrowed operand (`U8`, `U16`, `Upvalue`, `Strs`, `Captures`)
//! carries, in parentheses, what a program has too many of when the
//! operand does not fit. That is the text of the compile error, and the
//! only one for the limit: the compiler does not check sizes itself.
//!
//! # Effects
//!
//! `pops P, pushes Q` says the instruction takes `P` values off the top
//! of the frame and leaves `Q` there; an instruction that only looks at
//! the top value pops and pushes it. `cut C` between the two says that,
//! with the `P` values off, the frame is cut back to `C` values before
//! the `Q` are pushed. The expressions name the row's operands. A last
//! word says where control goes: `branch` (on, or to the target),
//! `jump` (to the target), `end` (nowhere: the function returns or
//! stops); without one, on to the next instruction.

use std::marker::PhantomData;

use super::{Const, Label, UpvalueDesc};
use crate::builtins::registry::BuiltinId;

// ── What a row is made of ──────────────────────────────────────────

/// What an instruction does to the height of its frame, and where
/// control goes after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effect {
    /// The values it takes off the top of the frame.
    pub pops: usize,
    /// With those off, the height the frame is cut back to.
    pub cut: Option<usize>,
    /// The values it then pushes.
    pub pushes: usize,
    pub flow: Flow,
}

/// Where control goes after an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// To the next instruction.
    Next,
    /// To the next instruction or to the target.
    Branch,
    /// To the target.
    Jump,
    /// Nowhere in this function.
    End,
}

/// The kind of constant an operand names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstKind {
    Any,
    Str,
    /// A variant's constructor.
    Tag,
    /// A type's descriptor.
    Type,
    /// A function.
    Func,
    Int,
    Float,
}

/// An operand of a decoded instruction with its kind, as the verifier
/// checks it.
#[derive(Debug, Clone, Copy)]
pub enum Operand {
    /// A count or an index into a value: nothing to check.
    Plain,
    Slot(usize),
    Upvalue(usize),
    Const(Const, ConstKind),
    Builtin(BuiltinId),
    Target(usize),
    Strs(Operands<Const>),
    Captures(Operands<UpvalueDesc>),
}

/// A list operand of a decoded instruction: where its items are in the
/// code.
#[derive(Debug, Clone, Copy)]
pub struct Operands<T> {
    at: usize,
    count: usize,
    item: PhantomData<T>,
}

/// An item of a list operand: two bytes of the code.
pub trait Packed: Copy {
    fn unpack(bytes: [u8; 2]) -> Self;
}

impl Packed for Const {
    fn unpack(bytes: [u8; 2]) -> Self {
        Const(u16::from_le_bytes(bytes))
    }
}

impl Packed for UpvalueDesc {
    fn unpack([is_local, index]: [u8; 2]) -> Self {
        UpvalueDesc {
            is_local: is_local != 0,
            index: usize::from(index),
        }
    }
}

impl<T: Packed> Operands<T> {
    /// The number of items.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The items, read from `code`: the code the instruction was decoded
    /// from.
    pub fn iter<'c>(&self, code: &'c [u8]) -> impl Iterator<Item = T> + 'c
    where
        T: 'c,
    {
        let (items, _) = code[self.at..self.at + self.count * 2].as_chunks::<2>();
        items.iter().map(|bytes| T::unpack(*bytes))
    }
}

/// An operand that does not fit its encoding: more of `what` than
/// `max`. Every limit of the bytecode is one of these, and reads
/// `too many <what>: <count> (the limit is <max>)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub what: &'static str,
    pub count: usize,
    pub max: usize,
}

/// What a function has too many of when a slot of its frame is past
/// the last one an operand can name.
pub(super) const FRAME_VALUES: &str = "values in the frame of a function \
     (its local bindings plus the values of the expression being evaluated)";

/// What an encoder writes to: the emitter, which knows where labels are.
pub(super) trait Writer {
    fn u8(&mut self, byte: u8);
    fn u16(&mut self, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        self.u8(lo);
        self.u8(hi);
    }
    /// The operand of a jump forward to `to`, which is not bound yet.
    fn fwd(&mut self, to: Label);
    /// The operand of a jump to `to`, forward or back.
    fn rel(&mut self, to: Label) -> Result<(), Limit>;
}

pub(super) fn narrow_u8(count: usize, what: &'static str) -> Result<u8, Limit> {
    u8::try_from(count).map_err(|_| Limit {
        what,
        count,
        max: usize::from(u8::MAX),
    })
}

fn narrow_u16(count: usize, what: &'static str) -> Result<u16, Limit> {
    u16::try_from(count).map_err(|_| Limit {
        what,
        count,
        max: usize::from(u16::MAX),
    })
}

/// A slot of the frame: one that does not fit means the frame holds
/// more values than slots can name.
fn slot_u16(slot: usize) -> Result<u16, Limit> {
    u16::try_from(slot).map_err(|_| Limit {
        what: FRAME_VALUES,
        count: slot + 1,
        max: usize::from(u16::MAX) + 1,
    })
}

/// The decoder's place in the code. Every read is checked: the verifier
/// decodes code nothing has checked yet.
struct Reader<'c> {
    code: &'c [u8],
    at: usize,
}

impl Reader<'_> {
    #[inline(always)]
    fn u8(&mut self) -> Option<u8> {
        let byte = *self.code.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    #[inline(always)]
    fn u16(&mut self) -> Option<u16> {
        let bytes = self.code.get(self.at..self.at + 2)?;
        self.at += 2;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    #[inline(always)]
    fn fwd(&mut self) -> Option<usize> {
        let distance = usize::from(self.u16()?);
        Some(self.at + distance)
    }

    #[inline(always)]
    fn rel(&mut self) -> Option<usize> {
        let bytes = self.code.get(self.at..self.at + 4)?;
        self.at += 4;
        let distance = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.at.checked_add_signed(isize::try_from(distance).ok()?)
    }

    #[inline(always)]
    fn list<T: Packed>(&mut self) -> Option<Operands<T>> {
        let count = usize::from(self.u8()?);
        let at = self.at;
        self.at = at + count * 2;
        (self.at <= self.code.len()).then_some(Operands {
            at,
            count,
            item: PhantomData,
        })
    }
}

// ── Operand kinds ──────────────────────────────────────────────────

/// The type of an operand of the kind in [`Asm`].
macro_rules! asm_ty {
    (U8, $lt:lifetime) => {
        usize
    };
    (U16, $lt:lifetime) => {
        usize
    };
    (Slot, $lt:lifetime) => {
        usize
    };
    (Cut, $lt:lifetime) => {
        usize
    };
    (Upvalue, $lt:lifetime) => {
        usize
    };
    (Global, $lt:lifetime) => {
        u16
    };
    (Trait, $lt:lifetime) => {
        u16
    };
    (Builtin, $lt:lifetime) => {
        BuiltinId
    };
    (Const, $lt:lifetime) => {
        Const
    };
    (Str, $lt:lifetime) => {
        Const
    };
    (Tag, $lt:lifetime) => {
        Const
    };
    (Type, $lt:lifetime) => {
        Const
    };
    (Func, $lt:lifetime) => {
        Const
    };
    (Int, $lt:lifetime) => {
        Const
    };
    (Float, $lt:lifetime) => {
        Const
    };
    (Fwd, $lt:lifetime) => {
        Label
    };
    (Rel, $lt:lifetime) => {
        Label
    };
    (Strs, $lt:lifetime) => {
        &$lt[Const]
    };
    (Captures, $lt:lifetime) => {
        &$lt[UpvalueDesc]
    };
}

/// The type of an operand of the kind in [`Instr`].
macro_rules! instr_ty {
    (U8) => { usize };
    (U16) => { usize };
    (Slot) => { usize };
    (Cut) => { usize };
    (Upvalue) => { usize };
    (Global) => { u16 };
    (Trait) => { u16 };
    (Builtin) => { BuiltinId };
    (Const) => { Const };
    (Str) => { Const };
    (Tag) => { Const };
    (Type) => { Const };
    (Func) => { Const };
    (Int) => { Const };
    (Float) => { Const };
    (Fwd) => { usize };
    (Rel) => { usize };
    (Strs) => { Operands<Const> };
    (Captures) => { Operands<UpvalueDesc> };
}

/// Write the operand `$v` of the kind to the writer `$w`.
macro_rules! encode_operand {
    (U8, $v:expr, $what:expr, $w:expr) => {
        $w.u8(narrow_u8($v, $what)?)
    };
    (U16, $v:expr, $what:expr, $w:expr) => {
        $w.u16(narrow_u16($v, $what)?)
    };
    (Slot, $v:expr, $what:expr, $w:expr) => {
        $w.u16(slot_u16($v)?)
    };
    (Cut, $v:expr, $what:expr, $w:expr) => {
        $w.u16(slot_u16($v)?)
    };
    (Upvalue, $v:expr, $what:expr, $w:expr) => {
        $w.u8(narrow_u8($v, $what)?)
    };
    (Global, $v:expr, $what:expr, $w:expr) => {
        $w.u16($v)
    };
    (Trait, $v:expr, $what:expr, $w:expr) => {
        $w.u16($v)
    };
    (Builtin, $v:expr, $what:expr, $w:expr) => {
        $w.u16($v.0)
    };
    (Const, $v:expr, $what:expr, $w:expr) => {
        $w.u16($v.0)
    };
    (Str, $v:expr, $what:expr, $w:expr) => {
        $w.u16($v.0)
    };
    (Tag, $v:expr, $what:expr, $w:expr) => {
        $w.u16($v.0)
    };
    (Type, $v:expr, $what:expr, $w:expr) => {
        $w.u16($v.0)
    };
    (Func, $v:expr, $what:expr, $w:expr) => {
        $w.u16($v.0)
    };
    (Int, $v:expr, $what:expr, $w:expr) => {
        $w.u16($v.0)
    };
    (Float, $v:expr, $what:expr, $w:expr) => {
        $w.u16($v.0)
    };
    (Fwd, $v:expr, $what:expr, $w:expr) => {
        $w.fwd($v)
    };
    (Rel, $v:expr, $what:expr, $w:expr) => {
        $w.rel($v)?
    };
    (Strs, $v:expr, $what:expr, $w:expr) => {{
        $w.u8(narrow_u8($v.len(), $what)?);
        for k in $v {
            $w.u16(k.0);
        }
    }};
    (Captures, $v:expr, $what:expr, $w:expr) => {{
        $w.u8(narrow_u8($v.len(), $what)?);
        for capture in $v {
            $w.u8(u8::from(capture.is_local));
            $w.u8(narrow_u8(
                capture.index,
                match capture.is_local {
                    true => "values in the frame below a local that a closure captures",
                    false => $what,
                },
            )?);
        }
    }};
}

/// Read an operand of the kind from the reader `$r`.
macro_rules! decode_operand {
    (U8, $r:expr) => {
        usize::from($r.u8()?)
    };
    (U16, $r:expr) => {
        usize::from($r.u16()?)
    };
    (Slot, $r:expr) => {
        usize::from($r.u16()?)
    };
    (Cut, $r:expr) => {
        usize::from($r.u16()?)
    };
    (Upvalue, $r:expr) => {
        usize::from($r.u8()?)
    };
    (Global, $r:expr) => {
        $r.u16()?
    };
    (Trait, $r:expr) => {
        $r.u16()?
    };
    (Builtin, $r:expr) => {
        BuiltinId($r.u16()?)
    };
    (Const, $r:expr) => {
        Const($r.u16()?)
    };
    (Str, $r:expr) => {
        Const($r.u16()?)
    };
    (Tag, $r:expr) => {
        Const($r.u16()?)
    };
    (Type, $r:expr) => {
        Const($r.u16()?)
    };
    (Func, $r:expr) => {
        Const($r.u16()?)
    };
    (Int, $r:expr) => {
        Const($r.u16()?)
    };
    (Float, $r:expr) => {
        Const($r.u16()?)
    };
    (Fwd, $r:expr) => {
        $r.fwd()?
    };
    (Rel, $r:expr) => {
        $r.rel()?
    };
    (Strs, $r:expr) => {
        $r.list::<Const>()?
    };
    (Captures, $r:expr) => {
        $r.list::<UpvalueDesc>()?
    };
}

/// The decoded operand `$v` of the kind, for the verifier.
macro_rules! checked_operand {
    (U8, $v:expr) => {
        Operand::Plain
    };
    (U16, $v:expr) => {
        Operand::Plain
    };
    (Slot, $v:expr) => {
        Operand::Slot($v)
    };
    (Cut, $v:expr) => {
        Operand::Plain
    };
    (Upvalue, $v:expr) => {
        Operand::Upvalue($v)
    };
    (Global, $v:expr) => {
        Operand::Plain
    };
    (Trait, $v:expr) => {
        Operand::Plain
    };
    (Builtin, $v:expr) => {
        Operand::Builtin($v)
    };
    (Const, $v:expr) => {
        Operand::Const($v, ConstKind::Any)
    };
    (Str, $v:expr) => {
        Operand::Const($v, ConstKind::Str)
    };
    (Tag, $v:expr) => {
        Operand::Const($v, ConstKind::Tag)
    };
    (Type, $v:expr) => {
        Operand::Const($v, ConstKind::Type)
    };
    (Func, $v:expr) => {
        Operand::Const($v, ConstKind::Func)
    };
    (Int, $v:expr) => {
        Operand::Const($v, ConstKind::Int)
    };
    (Float, $v:expr) => {
        Operand::Const($v, ConstKind::Float)
    };
    (Fwd, $v:expr) => {
        Operand::Target($v)
    };
    (Rel, $v:expr) => {
        Operand::Target($v)
    };
    (Strs, $v:expr) => {
        Operand::Strs($v)
    };
    (Captures, $v:expr) => {
        Operand::Captures($v)
    };
}

macro_rules! what {
    () => {
        ""
    };
    ($what:literal) => {
        $what
    };
}

macro_rules! cut {
    () => {
        None
    };
    ($cut:expr) => {
        Some($cut)
    };
}

macro_rules! flow {
    () => {
        Flow::Next
    };
    (branch) => {
        Flow::Branch
    };
    (jump) => {
        Flow::Jump
    };
    (end) => {
        Flow::End
    };
}

// ── The generator ──────────────────────────────────────────────────

macro_rules! ops {
    ($(
        $(#[$doc:meta])*
        $name:ident $( { $( $field:ident : $kind:ident $( ( $what:literal ) )? ),* } )?
            => pops $pops:expr, $( cut $cut:expr, )? pushes $pushes:expr $( , $flow:ident )? ;
    )*) => {
        /// An opcode: the first byte of an encoded instruction.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum Op {
            $( $(#[$doc])* $name, )*
        }

        impl Op {
            /// Every opcode, in the order of their bytes.
            pub const ALL: &'static [Op] = &[ $( Op::$name, )* ];

            /// The opcode `byte` is, if it is one.
            pub fn from_byte(byte: u8) -> Option<Op> {
                Self::ALL.get(usize::from(byte)).copied()
            }

            /// The opcode's name, as disassembly shows it.
            pub fn name(self) -> &'static str {
                match self {
                    $( Op::$name => stringify!($name), )*
                }
            }
        }

        /// An instruction as the compiler hands it to the emitter, with
        /// its operands at full width and its jumps to labels.
        #[derive(Debug, Clone, Copy)]
        pub enum Asm<'a> {
            $( $(#[$doc])* $name $( { $( $field: asm_ty!($kind, 'a), )* } )?, )*
        }

        impl Asm<'_> {
            pub fn op(&self) -> Op {
                match self {
                    $( Self::$name { .. } => Op::$name, )*
                }
            }

            /// What the instruction does to the frame.
            pub fn effect(&self) -> Effect {
                match *self {
                    $( Self::$name $( { $( $field, )* } )? => {
                        $( $( let _ = &$field; )* )?
                        Effect {
                            pops: $pops,
                            cut: cut!($($cut)?),
                            pushes: $pushes,
                            flow: flow!($($flow)?),
                        }
                    } )*
                }
            }

            /// Write the instruction: its opcode, then its operands,
            /// each narrowed to its encoded width.
            pub(super) fn encode(&self, w: &mut impl Writer) -> Result<(), Limit> {
                w.u8(self.op() as u8);
                match *self {
                    $( Self::$name $( { $( $field, )* } )? => {
                        $( $( encode_operand!($kind, $field, what!($($what)?), w); )* )?
                    } )*
                }
                Ok(())
            }
        }

        /// A decoded instruction.
        #[derive(Debug, Clone, Copy)]
        pub enum Instr {
            $( $(#[$doc])* $name $( { $( $field: instr_ty!($kind), )* } )?, )*
        }

        impl Instr {
            pub fn op(&self) -> Op {
                match self {
                    $( Self::$name { .. } => Op::$name, )*
                }
            }

            /// What the instruction does to the frame.
            pub fn effect(&self) -> Effect {
                match *self {
                    $( Self::$name $( { $( $field, )* } )? => {
                        $( $( let _ = &$field; )* )?
                        Effect {
                            pops: $pops,
                            cut: cut!($($cut)?),
                            pushes: $pushes,
                            flow: flow!($($flow)?),
                        }
                    } )*
                }
            }

            /// Hand each operand to `check`, with its kind.
            pub fn operands(&self, mut check: impl FnMut(Operand)) {
                match *self {
                    $( Self::$name $( { $( $field, )* } )? => {
                        $( $( let _ = &$field; )* )?
                        $( $( check(checked_operand!($kind, $field)); )* )?
                    } )*
                }
            }
        }

        /// The instruction encoded at `at` in `code` and the offset of
        /// the one after it; `None` when no instruction is encoded there
        /// (an unknown opcode, operands past the end of the code).
        #[inline(always)]
        pub fn decode(code: &[u8], at: usize) -> Option<(Instr, usize)> {
            let byte = *code.get(at)?;
            let mut r = Reader { code, at: at + 1 };
            let instr = match byte {
                $( b if b == Op::$name as u8 => {
                    $( $( let $field = decode_operand!($kind, r); )* )?
                    Instr::$name $( { $( $field, )* } )?
                } )*
                _ => return None,
            };
            Some((instr, r.at))
        }
    };
}

// ── The table ──────────────────────────────────────────────────────

ops! {
    // ── Constants & literals ────────────────────────────────────
    /// Push the constant.
    Constant { k: Const } => pops 0, pushes 1;
    /// Push Unit.
    Unit => pops 0, pushes 1;
    /// Push true.
    True => pops 0, pushes 1;
    /// Push false.
    False => pops 0, pushes 1;

    // ── Arithmetic ─────────────────────────────────────────────
    Add => pops 2, pushes 1;
    Sub => pops 2, pushes 1;
    Mul => pops 2, pushes 1;
    Div => pops 2, pushes 1;
    Mod => pops 2, pushes 1;

    // ── Comparison ─────────────────────────────────────────────
    Eq => pops 2, pushes 1;
    Neq => pops 2, pushes 1;
    Lt => pops 2, pushes 1;
    Gt => pops 2, pushes 1;
    Leq => pops 2, pushes 1;
    Geq => pops 2, pushes 1;

    // ── Unary ──────────────────────────────────────────────────
    Negate => pops 1, pushes 1;
    Not => pops 1, pushes 1;

    // ── String interpolation ───────────────────────────────────
    /// Concatenate the top `count` strings into one String.
    StringConcat { count: U8("segments of a string interpolation") } => pops count, pushes 1;
    /// Convert TOS to its Display string.
    DisplayValue => pops 1, pushes 1;

    // ── Variables ──────────────────────────────────────────────
    /// Push the value in the frame's slot.
    GetLocal { slot: Slot } => pops 0, pushes 1;
    /// Store TOS into the frame's slot. Does NOT pop.
    SetLocal { slot: Slot } => pops 1, pushes 1;
    /// Push the value of the global slot (see [`Globals`](super::Globals)).
    GetGlobal { slot: Global } => pops 0, pushes 1;
    /// Store TOS into the global slot. Does NOT pop.
    SetGlobal { slot: Global } => pops 1, pushes 1;

    // ── Upvalues (closures) ────────────────────────────────────
    /// Push the captured upvalue.
    GetUpvalue { index: Upvalue("values a closure captures") } => pops 0, pushes 1;

    // ── Function calls ─────────────────────────────────────────
    /// Call the function under the top `argc` values with them.
    Call { argc: U8("arguments of a call") } => pops argc + 1, pushes 1;
    /// Tail-call: reuse current frame. Followed by `Return`, which
    /// returns the result of a callee that is not a closure.
    TailCall { argc: U8("arguments of a call") } => pops argc + 1, pushes 1;
    /// Return TOS to caller.
    Return => pops 1, pushes 0, end;
    /// Call the builtin, a row of the builtin registry, with the top
    /// `argc` values.
    CallBuiltin { builtin: Builtin, argc: U8("arguments of a call") } => pops argc, pushes 1;

    // ── Closures ───────────────────────────────────────────────
    /// Create a closure of the function with the captured values.
    MakeClosure { f: Func, captures: Captures("values a closure captures") } => pops 0, pushes 1;

    // ── Data constructors ──────────────────────────────────────
    /// Create a tuple from the top `count` values.
    MakeTuple { count: U8("elements of a tuple") } => pops count, pushes 1;
    /// Create a list from the top `count` values.
    MakeList { count: U16("elements of a list literal") } => pops count, pushes 1;
    /// Create a map from the top `pairs` key-value pairs.
    MakeMap { pairs: U16("entries of a map literal") } => pops pairs * 2, pushes 1;
    /// Create a set from the top `count` values.
    MakeSet { count: U16("elements of a set literal") } => pops count, pushes 1;
    /// Create a record of the type whose descriptor the constant is,
    /// with the top values as the named fields, in order.
    MakeRecord { ty: Type, fields: Strs("fields of a record") } => pops fields.len(), pushes 1;
    /// Functional record update: the record under the top values with
    /// them as the named fields. The result has the base's type.
    RecordUpdate { fields: Strs("fields of a record update") } => pops fields.len() + 1, pushes 1;
    /// Create a lazy range (inclusive) from two ints on the stack.
    MakeRange => pops 2, pushes 1;
    /// Concatenate two lists/ranges on the stack into a single list.
    ListConcat => pops 2, pushes 1;

    // ── Field access ───────────────────────────────────────────
    /// Access the named field of TOS.
    GetField { name: Str } => pops 1, pushes 1;

    // ── Control flow ───────────────────────────────────────────
    /// Jump, forward or back.
    Jump { to: Rel } => pops 0, pushes 0, jump;
    /// Pop TOS; jump forward if falsy.
    JumpIfFalse { to: Fwd } => pops 1, pushes 0, branch;
    /// Pop TOS; jump forward if truthy.
    JumpIfTrue { to: Fwd } => pops 1, pushes 0, branch;
    /// Discard TOS.
    Pop => pops 1, pushes 0;
    /// Duplicate TOS.
    Dup => pops 1, pushes 2;

    // ── Pattern matching ───────────────────────────────────────
    /// Test if TOS is the variant whose constructor the constant is.
    /// Peek, push bool.
    TestTag { tag: Tag } => pops 1, pushes 2;
    /// Test if TOS equals the constant. Peek, push bool.
    TestEqual { k: Const } => pops 1, pushes 2;
    /// Test if TOS tuple has length `len`. Peek, push bool.
    TestTupleLen { len: U8("elements of a tuple pattern") } => pops 1, pushes 2;
    /// Test if TOS list has length >= `len`. Peek, push bool.
    TestListMin { len: U8("elements of a list pattern") } => pops 1, pushes 2;
    /// Test if TOS list has length == `len`. Peek, push bool.
    TestListExact { len: U8("elements of a list pattern") } => pops 1, pushes 2;
    /// Test if TOS int is in range [lo, hi]. Peek, push bool.
    TestIntRange { lo: Int, hi: Int } => pops 1, pushes 2;
    /// Test if TOS float is in range. Peek, push bool.
    TestFloatRange { lo: Float, hi: Float } => pops 1, pushes 2;
    /// Extract the tuple element at `index`. Peek tuple, push element.
    DestructTuple { index: U8("elements of a tuple pattern") } => pops 1, pushes 2;
    /// Extract the variant field at `index`. Peek variant, push field.
    DestructVariant { index: U8("fields of a variant pattern") } => pops 1, pushes 2;
    /// Extract the list element at `index`. Peek list, push element.
    DestructList { index: U8("elements of a list pattern") } => pops 1, pushes 2;
    /// Extract the list's tail from `start`. Peek list, push rest.
    DestructListRest { start: U8("elements of a list pattern") } => pops 1, pushes 2;
    /// Extract the named record field. Peek record, push value.
    DestructRecordField { name: Str } => pops 1, pushes 2;
    /// Construct a new record from TOS by removing the listed field
    /// names. The record on TOS is consumed (popped) and a new
    /// anonymous `Value::Record` of the fields minus the excluded ones is
    /// pushed. Used by row-polymorphic anon-record patterns to bind
    /// the `...rest` portion.
    DestructRecordRest { excluded: Strs("fields of an anonymous record pattern") } => pops 1, pushes 1;
    /// Test if TOS is a record of the type whose descriptor the
    /// constant is, or an anonymous record (see
    /// [`record_type_matches`](super::record_type_matches)). Peek, push
    /// bool.
    TestRecordTag { ty: Type } => pops 1, pushes 2;
    /// Test if TOS map contains the key. Peek, push bool.
    TestMapHasKey { key: Str } => pops 1, pushes 2;
    /// Extract the map value of the key. Peek map, push value.
    DestructMapValue { key: Str } => pops 1, pushes 2;

    // ── Loop ───────────────────────────────────────────────────
    /// Store the top `argc` values in the slots from `first` on and cut
    /// the frame back to just above them: the next round of a loop
    /// whose bindings those slots are. Followed by the jump back.
    Recur { argc: U8("bindings of a loop"), first: Cut } => pops argc, cut first, pushes argc;

    // ── Error handling ─────────────────────────────────────────
    /// Unwrap Ok/Some or early-return Err/None.
    QuestionMark => pops 1, pushes 1;
    /// Panic with message string on TOS.
    Panic => pops 1, pushes 0, end;

    /// Runtime method dispatch: the method named by the constant, of the
    /// impl of the trait `of` names (see
    /// [`Globals::call_method`](super::Globals::call_method)) for the
    /// receiver's type, else the builtin trait's native method; call it
    /// with the top `argc` values, the receiver first.
    CallMethod { method: Str, argc: U8("arguments of a method call, the receiver included"), of: Trait } => pops argc, pushes 1;
    /// `CallMethod` in tail position: a method that is a closure runs
    /// in the current frame. Followed by `Return`, which returns the
    /// result of any other.
    TailCallMethod { method: Str, argc: U8("arguments of a method call, the receiver included"), of: Trait } => pops argc, pushes 1;

    /// Move TOS down to the frame's slot `slot` and drop every value
    /// that was above that slot: pop TOS, cut the frame back to `slot`
    /// values, push the popped value. The compiler emits it where a
    /// scope ends with its locals still under the result, and where a
    /// failed pattern test lands, so that the frame again holds exactly
    /// the values the compiler has accounted for.
    Slide { slot: Cut } => pops 1, cut slot, pushes 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_below_the_opcode_count_is_its_opcode() {
        for byte in 0u8..=255 {
            match Op::from_byte(byte) {
                Some(op) => {
                    assert_eq!(op as u8, byte);
                    assert!(!op.name().is_empty());
                }
                None => assert!(usize::from(byte) >= Op::ALL.len()),
            }
        }
    }

    #[test]
    fn an_unknown_opcode_and_a_cut_off_operand_do_not_decode() {
        assert!(decode(&[255], 0).is_none());
        assert!(decode(&[Op::Constant as u8, 0], 0).is_none());
        assert!(decode(&[Op::MakeRecord as u8, 0, 0, 2, 0, 0], 0).is_none());
        // A jump to before the start of the code.
        let [a, b, c, d] = (-9i32).to_le_bytes();
        assert!(decode(&[Op::Jump as u8, a, b, c, d], 0).is_none());
        assert!(decode(&[], 0).is_none());
    }
}
