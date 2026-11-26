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
/// collection, the first argument, or of a record a `RecordUpdate`
/// after the call takes; and of that record's fields it replaces that
/// hold the collection (see the module's text). (A call, not a part of
/// the loop: see `interpolate`.)
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
    let Some(ahead) = chunk.ahead_at(ip, args_at - base) else {
        return;
    };
    let (slots, args) = stack.split_at_mut(args_at);
    let given = &args[0];
    let frame = &mut slots[base..];
    for at in ahead.unread() {
        if shares(&frame[at], given) {
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
            record.let_go_of_fields(names, |value| shares(value, given));
        }
    }
}

/// Whether `copy` is the map, the set or the list `arg` is.
fn shares(copy: &Value, arg: &Value) -> bool {
    match (copy, arg) {
        (Value::Map(copy), Value::Map(arg)) => copy.shares(arg),
        (Value::Set(copy), Value::Set(arg)) => copy.shares(arg),
        (Value::List(copy), Value::List(arg)) => copy.shares(arg),
        _ => false,
    }
}
