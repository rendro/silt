//! Before a call of a builtin that changes a collection where it is
//! when it is given the one holder of it (`map.set`, `map.update`,
//! `set.insert`, `list.append`, `list.prepend`, `list.set`:
//! [`crate::value::Map::put`]), and before a list literal joins two
//! lists (`[x, ..acc]`), the frame lets go of the copies of it
//! that the code after the call never reads again: the local it was
//! read from, most often. A fold's step ends in the call (`{ m, k ->
//! map.set(m, k, 1) }`); a function that calls itself hands the result
//! on (`go(i + 1, map.set(acc, i, i))`), or a record that holds it
//! (`go(rest, s.{ seen: set.insert(s.seen, x) })`).
//!
//! What the code after the call reads is the chunk's to say
//! ([`crate::bytecode::Chunk::ahead_at`]). A value no path reads is let
//! go of where it is when it is a copy of a collection the call is
//! given, or of a record whose replaced field holds one; and that field,
//! in the one record a `RecordUpdate` after the call replaces it in.
//! What is let go of lives on in the argument: nothing is dropped, and
//! nothing that is read again changes.

use crate::bytecode::{Chunk, Instr, decode};
use crate::value::Value;

pub(super) use crate::bytecode::ahead::puts;

/// Before a call of a builtin that changes a collection in place (see
/// [`puts`]), whose arguments start at `args_at` in the frame from
/// `base`, and after which the code goes on at `ip`: let go of each
/// value of the frame that is no more read and is a copy of the
/// collection, the first argument (see [`let_go`]). (A call, not a part
/// of the loop: see `interpolate`.)
#[inline(never)]
pub(super) fn let_go_of_copies(
    stack: &mut [Value],
    chunk: &Chunk,
    ip: usize,
    base: usize,
    args_at: usize,
) {
    if !matches!(
        stack.get(args_at),
        Some(Value::Map(_) | Value::Set(_) | Value::List(_))
    ) {
        return;
    }
    let_go(stack, chunk, ip, base, args_at, args_at..args_at + 1);
}

/// Before a `ListConcat` of the two lists from `args_at` (`[x, ..acc]`),
/// after which the code goes on at `ip`: let go of each value of the
/// frame that is no more read and is a copy of one of them (see
/// [`let_go`]), so that one the operands alone hold is changed in
/// place ([`crate::value::List::put_concat`]).
#[inline(never)]
pub(super) fn let_go_of_operands(
    stack: &mut [Value],
    chunk: &Chunk,
    ip: usize,
    base: usize,
    args_at: usize,
) {
    if !matches!(&stack[args_at..], [Value::List(_), Value::List(_)]) {
        return;
    }
    let_go(stack, chunk, ip, base, args_at, args_at..args_at + 2);
}

/// Before a call of a function, a method or a builtin as a value (in
/// tail position too: a builtin runs above the frame's values), that
/// takes the frame's values from `below` (its own slot, or its first
/// argument) and is given those from `args_at`, and after which the
/// code goes on at `ip`: when an argument is a collection or a record,
/// let go of each value of the frame that is no more read and is a copy
/// of one (see [`let_go`]). The function sees its arguments and
/// nothing of this frame: what it holds of the frame's values, a
/// closure's capture of one, it holds itself, and so keeps it from
/// being changed in place.
///
/// A closure that only reads the arguments it is given (see
/// [`crate::bytecode::Chunk::passes_on`]) changes none in place: the
/// frame keeps its copies, at no cost. So does a call after which the
/// frame reads all it holds.
#[inline(always)]
pub(super) fn before_call(
    stack: &mut [Value],
    chunk: &Chunk,
    ip: usize,
    base: usize,
    below: usize,
    args_at: usize,
) {
    // (No collection or record given, or a closure that hands on none
    // of what it is given: most calls ask no more.)
    if !stack[args_at..].iter().any(held) {
        return;
    }
    let passes = match stack.get(below) {
        Some(Value::VmClosure(callee)) if args_at == below + 1 && callee.bound.is_empty() => {
            match callee.function.chunk().passes_on(callee.function.arity()) {
                0 => return,
                passes => passes,
            }
        }
        _ => u64::MAX,
    };
    given_held(stack, chunk, ip, base, below, args_at, passes);
}

/// [`before_call`] of a call whose callee may hand on the arguments
/// `passes` (a bit each; past 64, they share bits).
#[inline(never)]
fn given_held(
    stack: &mut [Value],
    chunk: &Chunk,
    ip: usize,
    base: usize,
    below: usize,
    args_at: usize,
    passes: u64,
) {
    let mut given = 0u64;
    for (at, arg) in stack[args_at..].iter().enumerate() {
        given |= u64::from(held(arg)) << (at % 64);
    }
    if given & passes != 0 {
        let_go(stack, chunk, ip, base, below, args_at..stack.len());
    }
}

/// Whether the value is one a call may change in place when it holds
/// it alone: a collection, or a record that holds one.
fn held(value: &Value) -> bool {
    matches!(
        value,
        Value::Map(_) | Value::Set(_) | Value::List(_) | Value::Record(_)
    )
}

/// Let go of each value of the frame from `base` up to `below` that the
/// code from `ip` never reads again and that is a copy of one of the
/// values `given` (places of the stack); of a record a `RecordUpdate`
/// after the call takes, let go of its copies too, and of its fields the
/// update replaces that hold a given value. What is let go of lives on
/// in the arguments (or in the record the update takes): nothing is
/// dropped.
#[inline(never)]
fn let_go(
    stack: &mut [Value],
    chunk: &Chunk,
    ip: usize,
    base: usize,
    below: usize,
    given: std::ops::Range<usize>,
) {
    let Some(ahead) = chunk
        .ahead_at(ip, below - base)
        .filter(|ahead| !ahead.idle())
    else {
        return;
    };
    let (slots, rest) = stack.split_at_mut(below);
    let args = &rest[given.start - below..given.end - below];
    let given = |value: &Value| args.iter().any(|arg| shares(value, arg));
    let frame = &mut slots[base..];
    for at in ahead.unread() {
        if given(&frame[at]) {
            frame[at] = Value::Unit;
        }
    }
    // A record in a record (`o.{ inner: o.inner.{ m: map.set(o.inner.m,
    // k, v) } }`): the outer record's field holds the inner one, which
    // is the base of the update before, so the outer record is let go of
    // first; each round lets go of what the one before made the one
    // holder: as many rounds as there are records, at most.
    for _ in 0..ahead.bases().count() {
        let mut changed = false;
        for (at, update) in ahead.bases() {
            if !matches!(frame[at], Value::Record(_)) {
                continue;
            }
            for other in ahead.unread() {
                if let (Value::Record(copy), Value::Record(record)) = (&frame[other], &frame[at])
                    && copy.shares(record)
                {
                    frame[other] = Value::Unit;
                    changed = true;
                }
            }
            let Some((Instr::RecordUpdate { fields }, _)) = decode(chunk.code(), update) else {
                continue;
            };
            // A value of the frame that is this record and is read after
            // the call only by some of its fields (`o.{ inner: ..., n:
            // o.n + 1 }`) is let go of while the fields the update
            // replaces are let go of, but for those it reads, and is then
            // the record again: the same record, whose fields it reads
            // are as they were. (Dropped, it is a count; the record lives
            // on in the base.)
            let (mut kept, mut aside) = (0, 0u64);
            for (part, read) in ahead.partly_read() {
                if let (Value::Record(copy), Value::Record(record)) = (&frame[part], &frame[at])
                    && copy.shares(record)
                {
                    kept |= read;
                    aside |= 1 << part;
                    frame[part] = Value::Unit;
                }
            }
            let (below, rest) = frame.split_at_mut(at);
            let (record, above) = rest
                .split_first_mut()
                .expect("the base is a value of the frame");
            let Value::Record(record) = record else {
                continue;
            };
            // A field that holds a given value, or the record another
            // update takes as its base (which holds it on).
            let bases = |value: &Value| {
                ahead.bases().any(|(other, _)| {
                    other != at
                        && match other < at {
                            true => shares(value, &below[other]),
                            false => shares(value, &above[other - at - 1]),
                        }
                })
            };
            let names = fields.iter(chunk.code()).map(|k| chunk.string(k));
            changed |= record.let_go_of_fields(names, |value| given(value) || bases(value), kept);
            while aside != 0 {
                let part = aside.trailing_zeros() as usize;
                aside &= aside - 1;
                frame[part] = frame[at].clone();
            }
        }
        if !changed {
            break;
        }
    }
}

/// Whether `copy` is the map, the set, the list or the record `arg` is.
fn shares(copy: &Value, arg: &Value) -> bool {
    match (copy, arg) {
        (Value::Map(copy), Value::Map(arg)) => copy.shares(arg),
        (Value::Set(copy), Value::Set(arg)) => copy.shares(arg),
        (Value::List(copy), Value::List(arg)) => copy.shares(arg),
        (Value::Record(copy), Value::Record(arg)) => copy.shares(arg),
        _ => false,
    }
}
