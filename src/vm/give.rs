//! Before a call of a builtin that changes a collection where it is
//! when it is given the one holder of it (`map.set`, `map.update`,
//! `set.insert`, `list.append`, `list.set`:
//! [`crate::value::Map::put`]), the frame lets go of the copies of it
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

/// Before a call of a function, a method or a builtin as a value, that
/// takes the frame's values from `below` (its own slot, or its first
/// argument) and is given those from `args_at`, and after which the
/// code goes on at `ip`: when an argument is a collection or a record,
/// let go of each value of the frame that is no more read and is a copy
/// of one (see [`let_go`]). The function sees its arguments and
/// nothing of this frame: what it holds of the frame's values, a
/// closure's capture of one, it holds itself, and so keeps it from
/// being changed in place.
#[inline(always)]
pub(super) fn before_call(
    stack: &mut [Value],
    chunk: &Chunk,
    ip: usize,
    base: usize,
    below: usize,
    args_at: usize,
) {
    if stack[args_at..].iter().any(held) {
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
    let Some(ahead) = chunk.ahead_at(ip, below - base) else {
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
    for (at, update) in ahead.bases() {
        if !matches!(frame[at], Value::Record(_)) {
            continue;
        }
        for other in ahead.unread() {
            if let (Value::Record(copy), Value::Record(record)) = (&frame[other], &frame[at])
                && copy.shares(record)
            {
                frame[other] = Value::Unit;
            }
        }
        let Some((Instr::RecordUpdate { fields }, _)) = decode(chunk.code(), update) else {
            continue;
        };
        if let Value::Record(record) = &mut frame[at] {
            let names = fields.iter(chunk.code()).map(|k| chunk.string(k));
            record.let_go_of_fields(names, given);
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
