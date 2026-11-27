//! What the code after a call reads of the frame (see
//! `crate::vm::give`, which lets go of what it does not read).
//!
//! A builtin that changes a collection where it is when it is given
//! the one holder of it (`map.set`, `map.update`, `set.insert`,
//! `list.append`, `list.prepend`, `list.set`:
//! [`crate::value::Map::put`]) is called with the collection (and a
//! list literal that joins lists, `[x, ..acc]`, is given its lists), and the frame often holds a copy of it still:
//! the local it was read from. From the call on, the code is read along
//! every path up to the end of the frame (`Return`, a self tail call
//! `Recur`, `Panic`), and of each of the frame's values below the
//! call's arguments it is known whether a path reads it again. Code the
//! reading does not know, or more of it than [`AHEAD`] instructions,
//! gives no answer. What is read of a place is the same every time the
//! call is made there, so it is read once and kept with the chunk.

use std::sync::OnceLock;

use crate::builtins::registry::registry;
use crate::defs::BuiltinId;

use super::ops::{Flow, Instr, Operand, decode};

/// The most instructions the reading ahead reads, on all paths.
const AHEAD: usize = 64;

/// The builtins that change a collection they are given in place.
const PUTS: [&str; 6] = [
    "map.set",
    "map.update",
    "set.insert",
    "list.append",
    "list.prepend",
    "list.set",
];

/// Whether the builtin `id` is one that changes a collection in place.
#[inline]
pub(crate) fn puts(id: BuiltinId) -> bool {
    static IDS: OnceLock<Box<[bool]>> = OnceLock::new();
    let ids = IDS.get_or_init(|| {
        let rows: Vec<usize> = PUTS
            .iter()
            .filter_map(|name| registry().named(name))
            .map(|row| usize::from(row.id.0))
            .collect();
        let mut ids = vec![false; rows.iter().max().map_or(0, |most| most + 1)];
        for row in rows {
            ids[row] = true;
        }
        ids.into_boxed_slice()
    });
    ids.get(usize::from(id.0)).copied().unwrap_or(false)
}

/// Whether the builtin `id` may call a function it is given: one that
/// takes a function, as `list.fold` does, may hand its other arguments
/// to it, and that one change them in place. (One that takes none and
/// changes nothing in place only reads its arguments.)
#[inline]
pub(crate) fn calls_back(id: BuiltinId) -> bool {
    static IDS: OnceLock<Box<[bool]>> = OnceLock::new();
    let ids = IDS.get_or_init(|| {
        // (The ids are the places of a table, from 0 on.)
        (0..=u16::MAX)
            .map(|id| registry().builtin(BuiltinId(id)))
            .take_while(Option::is_some)
            .map(|row| row.is_none_or(|row| row.signature.contains("Fn(")))
            .collect()
    });
    ids.get(usize::from(id.0)).copied().unwrap_or(true)
}

/// The most values of the frame below a call's arguments that the
/// reading ahead follows: a frame of more lets go of none.
const PLACES: usize = 64;

/// The most values the reading ahead follows on the stack, the frame's
/// and those the code after the call puts on it.
const HIGH: usize = 96;

/// A value made after the call (the call's own among them), where a
/// place holds the number of the frame's value below the call's
/// arguments that it is.
const NEW: u8 = u8::MAX;

/// Added to the number of a frame's value: a copy of it that `GetLocal`
/// pushed after the call. Of a copy whose field is read at once
/// (`o.n`), only that field of the value is read; any other use of the
/// copy reads all of it.
const COPY: u8 = PLACES as u8;

/// The frame's value a place holds, the value itself or a copy of it.
fn of(was: u8) -> Option<u8> {
    (was != NEW).then_some(was % COPY)
}

/// The most of the frame's values of which the reading ahead keeps the
/// fields that are read, the value itself being read no other way:
/// past them, a field read reads all of the value.
const PARTS: usize = 4;

/// The stack as the reading ahead follows it: of each place, which of
/// the frame's values from before the call is there, or [`NEW`].
#[derive(Clone, Copy)]
struct Stack {
    len: usize,
    places: [u8; HIGH],
}

impl Stack {
    fn push(&mut self, was: u8) -> Option<()> {
        *self.places.get_mut(self.len)? = was;
        self.len += 1;
        Some(())
    }

    fn pop(&mut self) -> Option<u8> {
        self.len = self.len.checked_sub(1)?;
        Some(self.places[self.len])
    }

    fn get(&self, at: usize) -> Option<u8> {
        (at < self.len).then(|| self.places[at])
    }

    fn below(&self, len: usize) -> &[u8] {
        &self.places[..len.min(self.len)]
    }
}

/// The most records a `RecordUpdate` after the call takes from the
/// frame that the reading ahead keeps: past them it lets go of none.
const BASES: usize = 4;

/// What the code after a call of a builtin that changes a collection
/// in place does with the frame's values below the call's arguments:
/// which of them no path reads again, and which of them is a record
/// that a `RecordUpdate` after the call takes and no path reads
/// otherwise. (The same for every call at that place of the code: the
/// frame there has as many values on every path, as the verifier
/// checks.)
#[derive(Debug, Clone)]
pub(crate) struct Ahead {
    /// How many values the frame has below the call's arguments.
    olds: usize,
    /// Of each of them, a bit: whether no path reads it again, nor
    /// leaves it in place for the frame's next round, nor takes it as a
    /// record to update.
    unread: u64,
    /// The places of records that a `RecordUpdate` takes as its base,
    /// that no path reads otherwise, with the offset of that
    /// instruction: the same on every path that takes it.
    bases: [(u8, usize); BASES],
    taken: usize,
    /// The places of values no path reads but by fields (`o.n`),
    /// neither all of them nor as a record to update, with a bit for
    /// each field that is read.
    parts: [(u8, u64); PARTS],
    partly: usize,
}

impl Ahead {
    /// The places of the frame's values that no path reads but by some
    /// of their fields, with a bit for each field that is read.
    pub(crate) fn partly_read(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.parts[..self.partly]
            .iter()
            .map(|&(at, fields)| (usize::from(at), fields))
    }

    /// Whether the call lets go of nothing: no value is unread, partly
    /// read or the base of an update.
    #[inline]
    pub(crate) fn idle(&self) -> bool {
        self.unread == 0 && self.taken == 0 && self.partly == 0
    }

    /// The places of the frame's values (from its first) that no path
    /// reads again.
    pub(crate) fn unread(&self) -> impl Iterator<Item = usize> + use<> {
        let mut bits = self.unread;
        std::iter::from_fn(move || {
            let at = bits.trailing_zeros();
            bits &= bits.wrapping_sub(1);
            (at < u64::BITS).then_some(at as usize)
        })
    }

    /// The records a `RecordUpdate` takes from the frame: their places
    /// and the offsets of the update.
    pub(crate) fn bases(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.bases[..self.taken]
            .iter()
            .map(|&(at, update)| (usize::from(at), update))
    }
}

/// The paths of the code after a call, followed.
struct Follow {
    /// Of each of the frame's values below the call's arguments, a bit:
    /// whether a path reads it, or leaves it in place.
    read: u64,
    /// The records a `RecordUpdate` takes, with its offset, on each
    /// path.
    bases: [(u8, usize); BASES],
    taken: usize,
    /// The values of which a path reads a field of a copy, with a bit
    /// for each field.
    parts: [(u8, u64); PARTS],
    partly: usize,
    /// The instructions it may read yet.
    left: usize,
}

impl Follow {
    fn reads(&mut self, was: u8) {
        if let Some(at) = of(was) {
            self.read |= 1 << at;
        }
    }

    /// A path reads the field `index` of the frame's value `at`.
    fn reads_field(&mut self, at: u8, index: usize) {
        if index >= 64 {
            return self.reads(at);
        }
        match self.parts[..self.partly]
            .iter_mut()
            .find(|(part, _)| *part == at)
        {
            Some((_, fields)) => *fields |= 1 << index,
            None if self.partly < PARTS => {
                self.parts[self.partly] = (at, 1 << index);
                self.partly += 1;
            }
            None => self.reads(at),
        }
    }

    /// Follow the code from `ip` with `stack`, and each path it
    /// branches to: `None` when it meets an instruction it does not
    /// know, or has read [`AHEAD`] instructions.
    fn follow(&mut self, code: &[u8], mut ip: usize, mut stack: Stack) -> Option<()> {
        loop {
            self.left = self.left.checked_sub(1)?;
            let (instr, next) = decode(code, ip)?;
            match instr {
                // A copy is read where it is used.
                Instr::GetLocal { slot } => {
                    let was = stack.get(slot)?;
                    stack.push(of(was).map_or(NEW, |at| at + COPY))?;
                }
                Instr::GetField { index } if stack.get(stack.len.checked_sub(1)?)? != NEW => {
                    let was = stack.pop()?;
                    match was >= COPY {
                        true => self.reads_field(was - COPY, index),
                        false => self.reads(was),
                    }
                    stack.push(NEW)?;
                }
                Instr::MakeClosure { captures, .. } => {
                    for capture in captures.iter(code) {
                        if capture.is_local {
                            self.reads(stack.get(capture.index)?);
                        }
                    }
                    stack.push(NEW)?;
                }
                // A value let go of is not read.
                Instr::Pop => {
                    stack.pop()?;
                }
                Instr::Slide { slot } => {
                    self.reads(stack.pop()?);
                    if slot > stack.len {
                        return None;
                    }
                    stack.len = slot;
                    stack.push(NEW)?;
                }
                Instr::RecordUpdate { fields } => {
                    for _ in 0..fields.len() {
                        self.reads(stack.pop()?);
                    }
                    // (A copy pushed after the call reads its value.)
                    match stack.pop()? {
                        NEW => {}
                        base if base >= COPY => self.reads(base),
                        base => {
                            *self.bases.get_mut(self.taken)? = (base, ip);
                            self.taken += 1;
                        }
                    }
                    stack.push(NEW)?;
                }
                // The frame's first `keep` values stay where they are.
                Instr::TailCallKeep { argc, keep } => {
                    for _ in 0..=argc {
                        self.reads(stack.pop()?);
                    }
                    for &was in stack.below(keep) {
                        self.reads(was);
                    }
                    stack.push(NEW)?;
                }
                Instr::Return => {
                    self.reads(stack.pop()?);
                    return Some(());
                }
                // The values below the parameters stay where they are.
                Instr::Recur { argc, first, .. } => {
                    for _ in 0..usize::from(argc) {
                        self.reads(stack.pop()?);
                    }
                    for &was in stack.below(first) {
                        self.reads(was);
                    }
                    return Some(());
                }
                Instr::Panic => return Some(()),
                _ if takes_what_it_pops(&instr) => {
                    let effect = instr.effect();
                    for _ in 0..effect.pops {
                        self.reads(stack.pop()?);
                    }
                    for _ in 0..effect.pushes {
                        stack.push(NEW)?;
                    }
                    let mut target = None;
                    instr.operands(|operand| {
                        if let Operand::Target(to) = operand {
                            target = Some(to);
                        }
                    });
                    match effect.flow {
                        Flow::Next => {}
                        Flow::Branch => self.follow(code, target?, stack)?,
                        Flow::Jump => {
                            ip = target?;
                            continue;
                        }
                        Flow::End => return Some(()),
                    }
                }
                _ => return None,
            }
            ip = next;
        }
    }
}

/// Whether the instruction reads no value of the frame but those it
/// takes off it (each counted as read: one it looks at and puts back,
/// as a pattern's test does, too), and goes to no place but the next
/// instruction or its target. (`QuestionMark` may end the frame
/// instead, with the value it took.) A call of a function, a method or
/// a builtin sees the values it takes and nothing of the frame; a
/// closure that holds a value of the frame captured it, a read, when it
/// was made. `FrameDict` reads the dictionary below the frame, which is
/// none of the frame's values.
fn takes_what_it_pops(instr: &Instr) -> bool {
    matches!(
        instr,
        Instr::Constant { .. }
            | Instr::Unit
            | Instr::True
            | Instr::False
            | Instr::Add
            | Instr::Sub
            | Instr::Mul
            | Instr::Div
            | Instr::Mod
            | Instr::AddConst { .. }
            | Instr::SubConst { .. }
            | Instr::Eq
            | Instr::Neq
            | Instr::Lt
            | Instr::Gt
            | Instr::Leq
            | Instr::Geq
            | Instr::Negate
            | Instr::Not
            | Instr::Interpolate { .. }
            | Instr::DisplayValue
            | Instr::GetGlobal { .. }
            | Instr::GetUpvalue { .. }
            | Instr::GetClosure
            | Instr::Call { .. }
            | Instr::TailCall { .. }
            | Instr::CallBuiltin { .. }
            | Instr::MakeTuple { .. }
            | Instr::MakeList { .. }
            | Instr::MakeMap { .. }
            | Instr::MakeSet { .. }
            | Instr::MakeRecord { .. }
            | Instr::MakeRange
            | Instr::ListConcat
            | Instr::GetField { .. }
            | Instr::Jump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::JumpIfNotGt { .. }
            | Instr::JumpIfNotLeq { .. }
            | Instr::JumpIfNotGeq { .. }
            | Instr::JumpIfNotEq { .. }
            | Instr::JumpIfNotNeq { .. }
            | Instr::Dup
            | Instr::TestTag { .. }
            | Instr::TestEqual { .. }
            | Instr::TestTupleLen { .. }
            | Instr::TestListMin { .. }
            | Instr::TestListExact { .. }
            | Instr::TestIntRange { .. }
            | Instr::TestFloatRange { .. }
            | Instr::DestructTuple { .. }
            | Instr::DestructVariant { .. }
            | Instr::DestructList { .. }
            | Instr::DestructListRest { .. }
            | Instr::DestructRecordField { .. }
            | Instr::TestRecordTag { .. }
            | Instr::TestMapHasKey { .. }
            | Instr::DestructMapValue { .. }
            | Instr::QuestionMark
            | Instr::CallMethod { .. }
            | Instr::TailCallMethod { .. }
            | Instr::CallDict { .. }
            | Instr::TailCallDict { .. }
            | Instr::CallChecked { .. }
            | Instr::TailCallChecked { .. }
            | Instr::MakeType { .. }
            | Instr::MakeDict { .. }
            | Instr::DictMethod { .. }
            | Instr::DictSub { .. }
            | Instr::SuperDict { .. }
            | Instr::WhereDict { .. }
            | Instr::DictType
            | Instr::TypeArg { .. }
            | Instr::Bind { .. }
            | Instr::FrameDict
    )
}

/// What the code from `ip`, just after a call of a builtin with
/// `olds` values of the frame below its arguments, does with those:
/// `None` when it meets an instruction it does not know, or reads more
/// than [`AHEAD`] instructions, or the frame has more than [`PLACES`]
/// values below the arguments.
pub(crate) fn read(code: &[u8], ip: usize, olds: usize) -> Option<Ahead> {
    if olds > PLACES {
        return None;
    }
    let mut stack = Stack {
        len: 0,
        places: [NEW; HIGH],
    };
    for at in 0..olds {
        stack.push(at as u8)?;
    }
    stack.push(NEW)?;
    let mut follow = Follow {
        read: 0,
        bases: [(NEW, 0); BASES],
        taken: 0,
        parts: [(NEW, 0); PARTS],
        partly: 0,
        left: AHEAD,
    };
    follow.follow(code, ip, stack)?;
    // The bases no path reads otherwise, that each path that takes one
    // updates in the same instruction.
    let mut ahead = Ahead {
        olds,
        unread: 0,
        bases: [(NEW, 0); BASES],
        taken: 0,
        parts: [(NEW, 0); PARTS],
        partly: 0,
    };
    // (A value of which a field is read is read for a base, too.)
    let read = |at: u8| {
        follow.read & (1 << at) != 0
            || follow.parts[..follow.partly]
                .iter()
                .any(|(part, _)| *part == at)
    };
    for &(at, update) in &follow.bases[..follow.taken] {
        let same = |&(base, _): &(u8, usize)| base == at;
        let elsewhere = follow.bases[..follow.taken]
            .iter()
            .any(|&(base, other)| base == at && other != update);
        if read(at) || elsewhere || ahead.bases[..ahead.taken].iter().any(same) {
            continue;
        }
        ahead.bases[ahead.taken] = (at, update);
        ahead.taken += 1;
    }
    for &(at, fields) in &follow.parts[..follow.partly] {
        let based = follow.bases[..follow.taken]
            .iter()
            .any(|&(base, _)| base == at);
        if follow.read & (1 << at) == 0 && !based {
            ahead.parts[ahead.partly] = (at, fields);
            ahead.partly += 1;
        }
    }
    for at in 0..olds {
        let based = ahead.bases[..ahead.taken]
            .iter()
            .any(|&(base, _)| usize::from(base) == at);
        if !read(at as u8) && !based {
            ahead.unread |= 1 << at;
        }
    }
    Some(ahead)
}

/// The calls of a chunk whose code after them is read ahead: of each
/// place in the code, the number of its site plus one (0 for a place
/// just after no such call), and the sites, each with what the code
/// after the call reads, once a call there has been made
/// ([`super::Chunk::ahead_at`]).
#[derive(Debug, Clone)]
pub(crate) struct Sites {
    of: Box<[u32]>,
    sites: Box<[OnceLock<Option<Ahead>>]>,
}

impl Sites {
    /// The places in `code` just after each call of a builtin that
    /// changes a collection in place, and of each call of a function or
    /// a method.
    pub(crate) fn of(code: &[u8]) -> Sites {
        let mut of = vec![0u32; code.len() + 1];
        let mut sites = Vec::new();
        let mut at = 0;
        while let Some((instr, next)) = decode(code, at) {
            let site = match instr {
                Instr::CallBuiltin { builtin, .. } => puts(builtin),
                // (`[x, ..acc]`: `crate::vm::give::let_go_of_operands`.)
                Instr::ListConcat
                | Instr::Call { .. }
                | Instr::TailCall { .. }
                | Instr::CallChecked { .. }
                | Instr::CallMethod { .. }
                | Instr::CallDict { .. } => true,
                _ => false,
            };
            if site && let Ok(number) = u32::try_from(sites.len() + 1) {
                of[next] = number;
                sites.push(OnceLock::new());
            }
            at = next;
        }
        Sites {
            of: of.into_boxed_slice(),
            sites: sites.into_boxed_slice(),
        }
    }

    /// What the code from `ip` reads of a frame of `olds` values below
    /// the call's arguments, read at the first call.
    #[inline]
    pub(crate) fn at(&self, code: &[u8], ip: usize, olds: usize) -> Option<&Ahead> {
        let number = *self.of.get(ip)?;
        let site = self
            .sites
            .get(usize::try_from(number).ok()?.checked_sub(1)?)?;
        let ahead = site.get_or_init(|| read(code, ip, olds)).as_ref()?;
        (ahead.olds == olds).then_some(ahead)
    }
}

/// The most instructions [`passes_on`] reads of a function, on all paths.
const PASSES: usize = 4096;

/// Of each parameter of a function of `arity` parameters whose code is
/// `code`, a bit: whether the function may hand the value it is given
/// on to a call, a builtin, a record update, a closure or a value it
/// makes, where it could be changed in place; a parameter of which it
/// only reads (matches it, reads its fields, compares it, returns it)
/// has none. All bits when the code is more than [`PASSES`]
/// instructions on its paths, or of an instruction the reading does not
/// know. (A bit too many is safe: the caller lets go of its copies for
/// nothing. A bit too few is too: the call's update copies.)
pub(crate) fn passes_on(code: &[u8], arity: usize) -> u64 {
    if arity > PLACES {
        return u64::MAX;
    }
    let mut stack = Stack {
        len: 0,
        places: [NEW; HIGH],
    };
    for at in 0..arity {
        if stack.push(at as u8).is_none() {
            return u64::MAX;
        }
    }
    let mut pass = Pass {
        on: 0,
        into: [0; PLACES],
        left: PASSES,
    };
    if pass.follow(code, 0, stack, arity).is_none() {
        return u64::MAX;
    }
    // A parameter handed to the next round as another one is handed on
    // where that one is.
    let mut on = pass.on;
    loop {
        let more = (0..arity)
            .filter(|&p| pass.into[p] & on != 0)
            .fold(on, |on, p| on | 1 << p);
        if more == on {
            return on;
        }
        on = more;
    }
}

/// The paths of a function's code, followed for [`passes_on`].
struct Pass {
    /// The parameters handed on.
    on: u64,
    /// Of each parameter, the parameters a self tail call makes it.
    into: [u64; PLACES],
    left: usize,
}

impl Pass {
    fn passes(&mut self, was: u8) {
        if let Some(at) = of(was) {
            self.on |= 1 << at;
        }
    }

    fn follow(&mut self, code: &[u8], mut ip: usize, mut stack: Stack, arity: usize) -> Option<()> {
        loop {
            self.left = self.left.checked_sub(1)?;
            let (instr, next) = decode(code, ip)?;
            let effect = instr.effect();
            let mut target = None;
            instr.operands(|operand| {
                if let Operand::Target(to) = operand {
                    target = Some(to);
                }
            });
            match instr {
                Instr::GetLocal { slot } => {
                    let was = stack.get(slot)?;
                    stack.push(of(was).unwrap_or(NEW))?;
                }
                Instr::SetLocal { slot } => {
                    let top = stack.get(stack.len.checked_sub(1)?)?;
                    *stack.places.get_mut(slot)? = top;
                }
                Instr::Pop => {
                    stack.pop()?;
                }
                Instr::Slide { slot } => {
                    let top = stack.pop()?;
                    if slot > stack.len {
                        return None;
                    }
                    stack.len = slot;
                    stack.push(top)?;
                }
                Instr::Return | Instr::Panic => return Some(()),
                Instr::Recur { argc, first, .. } => {
                    let argc = usize::from(argc);
                    for i in (0..argc).rev() {
                        let was = stack.pop()?;
                        match (of(was), first + i < arity) {
                            (Some(p), true) => self.into[usize::from(p)] |= 1 << (first + i),
                            (Some(_), false) => self.passes(was),
                            (None, _) => {}
                        }
                    }
                    return Some(());
                }
                // A pattern's test and part, a copy: the value stays.
                Instr::TestTag { .. }
                | Instr::TestEqual { .. }
                | Instr::TestTupleLen { .. }
                | Instr::TestListMin { .. }
                | Instr::TestListExact { .. }
                | Instr::TestIntRange { .. }
                | Instr::TestFloatRange { .. }
                | Instr::TestRecordTag { .. }
                | Instr::TestMapHasKey { .. }
                | Instr::DestructTuple { .. }
                | Instr::DestructVariant { .. }
                | Instr::DestructList { .. }
                | Instr::DestructListRest { .. }
                | Instr::DestructRecordField { .. }
                | Instr::DestructMapValue { .. }
                | Instr::Dup => {
                    let was = stack.pop()?;
                    stack.push(was)?;
                    stack.push(match instr {
                        Instr::Dup => was,
                        _ => NEW,
                    })?;
                }
                // What only reads the values it takes.
                Instr::Constant { .. }
                | Instr::Unit
                | Instr::True
                | Instr::False
                | Instr::Add
                | Instr::Sub
                | Instr::Mul
                | Instr::Div
                | Instr::Mod
                | Instr::AddConst { .. }
                | Instr::SubConst { .. }
                | Instr::Eq
                | Instr::Neq
                | Instr::Lt
                | Instr::Gt
                | Instr::Leq
                | Instr::Geq
                | Instr::Negate
                | Instr::Not
                | Instr::GetGlobal { .. }
                | Instr::GetUpvalue { .. }
                | Instr::GetClosure
                | Instr::FrameDict
                | Instr::GetField { .. }
                | Instr::Jump { .. }
                | Instr::JumpIfFalse { .. }
                | Instr::JumpIfTrue { .. }
                | Instr::JumpIfNotLt { .. }
                | Instr::JumpIfNotGt { .. }
                | Instr::JumpIfNotLeq { .. }
                | Instr::JumpIfNotGeq { .. }
                | Instr::JumpIfNotEq { .. }
                | Instr::JumpIfNotNeq { .. } => {
                    for _ in 0..effect.pops {
                        stack.pop()?;
                    }
                    for _ in 0..effect.pushes {
                        stack.push(NEW)?;
                    }
                }
                Instr::MakeClosure { captures, .. } => {
                    for capture in captures.iter(code) {
                        if capture.is_local {
                            self.passes(stack.get(capture.index)?);
                        }
                    }
                    stack.push(NEW)?;
                }
                // A builtin that changes nothing in place and calls no
                // function only reads what it is given.
                Instr::CallBuiltin { builtin, argc } if !puts(builtin) && !calls_back(builtin) => {
                    for _ in 0..argc {
                        stack.pop()?;
                    }
                    stack.push(NEW)?;
                }
                // Anything else hands on what it takes.
                _ if effect.cut.is_none()
                    && matches!(effect.flow, Flow::Next | Flow::Branch | Flow::Jump) =>
                {
                    for _ in 0..effect.pops {
                        self.passes(stack.pop()?);
                    }
                    for _ in 0..effect.pushes {
                        stack.push(NEW)?;
                    }
                }
                _ => return None,
            }
            match effect.flow {
                Flow::Next => ip = next,
                Flow::Branch => {
                    self.follow(code, target?, stack, arity)?;
                    ip = next;
                }
                Flow::Jump => ip = target?,
                Flow::End => return Some(()),
            }
        }
    }
}
