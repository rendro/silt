use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use super::{MAX_RANGE_MATERIALIZE, Value};
use crate::typeinfo::ty;

/// Compare a named field in two record field maps.
fn cmp_record_field(
    a: &BTreeMap<String, Value>,
    b: &BTreeMap<String, Value>,
    key: &str,
) -> Ordering {
    match (a.get(key), b.get(key)) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

/// Materialized length of the inclusive range `lo..=hi`, clamped to 0 when
/// empty and saturating to `i64::MAX` for ranges larger than `i64::MAX`
/// elements (e.g. `i64::MIN..=i64::MAX`). Computed via `i128` to avoid
/// overflow on the subtraction/addition. L2 fix: the old implementation
/// computed `hi - lo + 1` directly in i64, which panicked in debug and
/// wrapped in release builds for extreme ranges.
fn range_len(lo: i64, hi: i64) -> i64 {
    if lo > hi {
        return 0;
    }
    let len = (hi as i128) - (lo as i128) + 1;
    if len > i64::MAX as i128 {
        i64::MAX
    } else {
        len as i64
    }
}

/// Compare a `List` and a `Range` for equality. Returns `true` when the list
/// has exactly the same materialized elements as the range (all `Int`s in
/// ascending order from `lo` to `hi` inclusive).
fn list_eq_range(list: &[Value], lo: i64, hi: i64) -> bool {
    let len = range_len(lo, hi);
    if list.len() as i64 != len {
        return false;
    }
    if len == 0 {
        return true;
    }
    // Zip the list against an increasing counter so the intent (one
    // list item per integer in [lo, hi]) is structural.
    for (cur, item) in (lo..=hi).zip(list.iter()) {
        match item {
            Value::Int(n) if *n == cur => {}
            _ => return false,
        }
    }
    true
}

/// Lexicographically compare a `List` and a `Range`.
///
/// Treats the range as its materialized sequence of `Int`s from `lo` to `hi`
/// inclusive. When `list_first` is true, `list` is the left-hand side; when
/// false, the range is the left-hand side and the resulting ordering is
/// reversed accordingly.
pub(crate) fn cmp_list_range(list: &[Value], lo: i64, hi: i64, list_first: bool) -> Ordering {
    let range_len = range_len(lo, hi);
    let common = (list.len() as i64).min(range_len);
    for i in 0..common {
        let range_val = Value::Int(lo + i);
        let list_item = &list[i as usize];
        let ord = if list_first {
            list_item.cmp(&range_val)
        } else {
            range_val.cmp(list_item)
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    // Shared prefix is equal — the shorter side is less.
    let list_len = list.len() as i64;
    let len_ord = list_len.cmp(&range_len);
    if list_first {
        len_ord
    } else {
        len_ord.reverse()
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Tuple(a), Value::Tuple(b)) => a == b,
            (Value::Variant(na, fa), Value::Variant(nb, fb)) => na == nb && fa == fb,
            (Value::Unit, Value::Unit) => true,
            (Value::List(a), Value::List(b)) => a.as_slice() == b.as_slice(),
            (Value::Range(a1, a2), Value::Range(b1, b2)) => {
                // Two ranges are equal iff they materialize to the same
                // sequence. Empty ranges (`lo > hi`) are all equal to each
                // other regardless of their endpoints.
                let (a_lo, a_hi) = (*a1, *a2);
                let (b_lo, b_hi) = (*b1, *b2);
                let a_empty = a_lo > a_hi;
                let b_empty = b_lo > b_hi;
                if a_empty || b_empty {
                    a_empty && b_empty
                } else {
                    a_lo == b_lo && a_hi == b_hi
                }
            }
            // Range vs List: the typechecker gives `Range(..)` the type
            // `List(Int)`, so the two sides share a Silt type and must have
            // a defined equality. Walk the range and list element-wise.
            (Value::List(list), Value::Range(lo, hi)) => list_eq_range(list.as_slice(), *lo, *hi),
            (Value::Range(lo, hi), Value::List(list)) => list_eq_range(list.as_slice(), *lo, *hi),
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Set(a), Value::Set(b)) => a == b,
            // The typechecker lets a nominal record and an anonymous
            // record of the same shape meet (`unify_anon_nominal`) with
            // no change to the value, so when either side is anonymous
            // the fields alone decide. Two nominal types (`Person{x:1}`
            // vs `Car{x:1}`) are never unified and compare unequal.
            (Value::Record(ta, fa), Value::Record(tb, fb)) => {
                if ta.is_anon() || tb.is_anon() {
                    fa == fb
                } else {
                    ta.id == tb.id && fa == fb
                }
            }
            (Value::TypeDescriptor(a), Value::TypeDescriptor(b)) => a.id == b.id,
            (Value::PrimitiveDescriptor(a), Value::PrimitiveDescriptor(b)) => a == b,
            (Value::Channel(a), Value::Channel(b)) => a.id() == b.id(),
            // Structural equality — same content, regardless of Arc identity.
            // This is the load-bearing forward-compat decision for the
            // future native `Type::Bytes`: equality semantics must already
            // match what a value-type byte array would do.
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            // Tcp handles: identity-based, like Channel/Handle.
            (Value::TcpListener(a), Value::TcpListener(b)) => a.id == b.id,
            (Value::TcpStream(a), Value::TcpStream(b)) => a.id == b.id,
            // Handle / VmClosure / BuiltinFn / VariantConstructor: without
            // explicit arms here, the catch-all `_ => false` would violate
            // reflexivity (`h == h` returning false) and break the Eq/Ord
            // contract — `impl Ord` (below) returns `Equal` for identical
            // instances while `PartialEq` returned `false`, so BTreeSet /
            // BTreeMap silently dropped duplicates. Mirror the identity
            // rules from `impl Ord`:
            //   - Handle: id equality (TaskHandle is heap-allocated, id is unique).
            //   - VmClosure: Arc pointer equality (closures carry captured
            //     upvalues; two closures of the same function with different
            //     upvalues must NOT compare equal).
            //   - BuiltinFn: the same row of the builtin registry.
            //   - VariantConstructor: variant equality.
            // Cross-kind pairs still fall through to `_ => false`.
            (Value::Handle(a), Value::Handle(b)) => a.id == b.id,
            (Value::VmClosure(a), Value::VmClosure(b)) => Arc::ptr_eq(a, b),
            (Value::BuiltinFn(a), Value::BuiltinFn(b)) => a == b,
            (Value::HostFn(a), Value::HostFn(b)) => a.name == b.name,
            (Value::VariantConstructor(a), Value::VariantConstructor(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        let disc = |v: &Value| -> u8 {
            match v {
                Value::Unit => 0,
                Value::Bool(_) => 1,
                Value::Int(_) => 2,
                Value::Float(_) => 3,
                Value::String(_) => 5,
                Value::List(_) => 6,
                Value::Range(..) => 6, // same discriminant as List for ordering
                Value::Tuple(_) => 7,
                Value::Map(_) => 8,
                Value::Set(_) => 9,
                Value::Record(..) => 10,
                Value::Variant(..) => 11,
                Value::Channel(_) => 12,
                Value::Handle(_) => 13,
                Value::VmClosure(_) => 14,
                Value::BuiltinFn(_) => 15,
                Value::HostFn(_) => 22,
                Value::VariantConstructor(..) => 16,
                Value::TypeDescriptor(_) => 17,
                Value::PrimitiveDescriptor(_) => 18,
                Value::Bytes(_) => 19,
                Value::TcpListener(_) => 20,
                Value::TcpStream(_) => 21,
            }
        };
        let d1 = disc(self);
        let d2 = disc(other);
        if d1 != d2 {
            return d1.cmp(&d2);
        }
        match (self, other) {
            (Value::Unit, Value::Unit) => Ordering::Equal,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => {
                // Float values are guaranteed finite, so partial_cmp always
                // returns Some. The fallback to Equal is a safety net that
                // keeps Eq/Ord consistent (NaN == NaN) if a non-finite value
                // ever appears.
                a.partial_cmp(b).unwrap_or(Ordering::Equal)
            }
            (Value::String(a), Value::String(b)) => a.cmp(b),
            (Value::List(a), Value::List(b)) => a.as_slice().cmp(b.as_slice()),
            (Value::Range(a1, a2), Value::Range(b1, b2)) => {
                // Lexicographically compare materialized ranges. Empty ranges
                // (lo > hi) are all equal regardless of endpoints.
                let (al, ah) = (*a1, *a2);
                let (bl, bh) = (*b1, *b2);
                let a_len = range_len(al, ah);
                let b_len = range_len(bl, bh);
                if a_len == 0 || b_len == 0 {
                    a_len.cmp(&b_len)
                } else {
                    al.cmp(&bl).then_with(|| a_len.cmp(&b_len))
                }
            }
            // Range vs List: walk element-wise. Required for Ord/PartialOrd
            // consistency with PartialEq when the typechecker hands both
            // sides the same `List(Int)` type.
            (Value::List(list), Value::Range(lo, hi)) => {
                cmp_list_range(list.as_slice(), *lo, *hi, true)
            }
            (Value::Range(lo, hi), Value::List(list)) => {
                cmp_list_range(list.as_slice(), *lo, *hi, false)
            }
            (Value::Tuple(a), Value::Tuple(b)) => a.cmp(b),
            (Value::Map(a), Value::Map(b)) => a.iter().cmp(b.iter()),
            (Value::Set(a), Value::Set(b)) => a.iter().cmp(b.iter()),
            (Value::Record(ta, fa), Value::Record(tb, fb)) => {
                // Mirror the `<anon>` wildcard of `PartialEq`: when either
                // side is an anonymous record, the typechecker has
                // already decided these are the same type, and Ord must
                // agree so that `a == b ⇒ cmp(a, b) == Equal`; otherwise
                // BTreeSet / BTreeMap would treat equal values as
                // distinct. Records of one builtin time type order by
                // their fields from the largest unit down; a record of
                // a declared type by its fields in the order the type
                // declares them; an anonymous record, which has no
                // declaration, by its fields in name order.
                if ta.is_anon() || tb.is_anon() {
                    fa.iter().cmp(fb.iter())
                } else {
                    ta.id.cmp(&tb.id).then_with(|| match ta.id {
                        ty::DATE => cmp_record_field(fa, fb, "year")
                            .then_with(|| cmp_record_field(fa, fb, "month"))
                            .then_with(|| cmp_record_field(fa, fb, "day")),
                        ty::TIME => cmp_record_field(fa, fb, "hour")
                            .then_with(|| cmp_record_field(fa, fb, "minute"))
                            .then_with(|| cmp_record_field(fa, fb, "second"))
                            .then_with(|| cmp_record_field(fa, fb, "ns")),
                        ty::DATE_TIME => cmp_record_field(fa, fb, "date")
                            .then_with(|| cmp_record_field(fa, fb, "time")),
                        _ => match &ta.shape {
                            crate::typeinfo::Shape::Record(declared) if !declared.is_empty() => {
                                declared
                                    .iter()
                                    .map(|(name, _)| cmp_record_field(fa, fb, name))
                                    .find(|ordering| ordering.is_ne())
                                    .unwrap_or(Ordering::Equal)
                            }
                            _ => fa.iter().cmp(fb.iter()),
                        },
                    })
                }
            }
            // Variants of one enum order by declaration, then by their
            // fields (see `Tag`'s `Ord`).
            (Value::Variant(ta, fa), Value::Variant(tb, fb)) => ta.cmp(tb).then_with(|| fa.cmp(fb)),
            (Value::TypeDescriptor(a), Value::TypeDescriptor(b)) => a.id.cmp(&b.id),
            (Value::PrimitiveDescriptor(a), Value::PrimitiveDescriptor(b)) => a.cmp(b),
            (Value::Channel(a), Value::Channel(b)) => a.id().cmp(&b.id()),
            // Structural lex comparison on bytes — required for Eq/Ord
            // consistency with structural PartialEq above. BTreeMap/BTreeSet
            // key contracts depend on this.
            (Value::Bytes(a), Value::Bytes(b)) => a.as_slice().cmp(b.as_slice()),
            // Tcp handles ordered by id (identity), matching PartialEq.
            (Value::TcpListener(a), Value::TcpListener(b)) => a.id.cmp(&b.id),
            (Value::TcpStream(a), Value::TcpStream(b)) => a.id.cmp(&b.id),
            // Handle / VmClosure / BuiltinFn / VariantConstructor: PartialEq
            // returns `false` for every pair (catch-all `_ => false` arm at
            // ~line 1028), so Ord must never return `Equal` for distinct
            // instances either — otherwise BTreeSet / BTreeMap silently drop
            // what they see as duplicates (Ord contract: a == b ⇒ cmp == Equal,
            // contrapositively a != b ⇒ cmp != Equal). We order by identity
            // (`id` field for TaskHandle, Arc pointer address for VmClosure)
            // and by contents for the name-carrying variants.
            (Value::Handle(a), Value::Handle(b)) => a.id.cmp(&b.id),
            (Value::VmClosure(a), Value::VmClosure(b)) => {
                (Arc::as_ptr(a) as usize).cmp(&(Arc::as_ptr(b) as usize))
            }
            (Value::BuiltinFn(a), Value::BuiltinFn(b)) => a.cmp(b),
            (Value::HostFn(a), Value::HostFn(b)) => a.name.cmp(&b.name),
            (Value::VariantConstructor(a), Value::VariantConstructor(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Explicit per-category tags (NOT `std::mem::discriminant(self)`):
        // the Hash/Eq contract requires that `a == b ⇒ hash(a) == hash(b)`,
        // and `PartialEq` admits a cross-discriminant equal pair:
        // `List(xs) == Range(lo, hi)` when xs materializes the range.
        // Therefore List and Range MUST share a tag AND hash the same
        // materialized sequence.
        match self {
            Value::Unit => {
                state.write_u8(0);
            }
            Value::Bool(b) => {
                state.write_u8(1);
                b.hash(state);
            }
            Value::Int(n) => {
                state.write_u8(2);
                n.hash(state);
            }
            // Canonicalize -0.0 → 0.0 so that 0.0/-0.0 (which compare
            // equal) hash equally.
            Value::Float(f) => {
                state.write_u8(3);
                let bits = if *f == 0.0 {
                    0.0_f64.to_bits()
                } else {
                    f.to_bits()
                };
                bits.hash(state);
            }
            Value::String(s) => {
                state.write_u8(4);
                s.hash(state);
            }
            // List and Range share tag 5 (any-list-shape). Range hashes its
            // materialized `[Int(lo), Int(lo+1), ..., Int(hi)]` sequence so
            // the contract holds: a `List` and a `Range` that compare equal
            // produce the same byte stream into the hasher.
            Value::List(xs) => {
                state.write_u8(5);
                xs.len().hash(state);
                for x in xs.iter() {
                    x.hash(state);
                }
            }
            // Round 92 (BROKEN): the element-by-element walk below used to be
            // unconditional, so `(0..4_000_000_000).hash()` spun ~4e9
            // iterations inside a single opcode — uninterruptible by the
            // time-slice scheduler — and `0..i64::MAX` hung forever. The walk
            // is now capped at `MAX_RANGE_MATERIALIZE`; over-cap ranges hash a
            // closed form of their endpoints instead. This preserves the
            // Hash/Eq contract (`a == b ⇒ hash(a) == hash(b)`, round 74):
            //   - within the cap: identical byte stream to the equal `List`
            //     (`tag 5, len, Int(lo) .. Int(hi)`), so List ↔ Range equal
            //     pairs still hash equal;
            //   - over the cap: every list-producing site enforces
            //     `MAX_RANGE_MATERIALIZE` (vm/iter.rs, builtins/*), so no
            //     `Value::List` can ever have > cap elements and no List can
            //     compare equal to an over-cap Range. The only values equal
            //     to such a Range are Ranges, and non-empty equal Ranges have
            //     identical endpoints (see `PartialEq` ~line 1693), so
            //     hashing `(len, lo, hi)` is contract-safe. (Empty ranges all
            //     compare equal regardless of endpoints; they take the
            //     `len == 0` path and hash only `(tag, 0)`, as before.)
            // Doing the cap inside `impl Hash` (rather than erroring in the
            // dispatch arm) also bounds nested walks for free: `.hash()`
            // on records/variants/tuples/lists recurses into this arm for
            // embedded range fields.
            Value::Range(lo, hi) => {
                state.write_u8(5);
                let len = range_len(*lo, *hi);
                (len as usize).hash(state);
                if len > 0 {
                    if len as u128 <= MAX_RANGE_MATERIALIZE as u128 {
                        for n in *lo..=*hi {
                            Value::Int(n).hash(state);
                        }
                    } else {
                        lo.hash(state);
                        hi.hash(state);
                    }
                }
            }
            Value::Tuple(vs) => {
                state.write_u8(6);
                vs.len().hash(state);
                for v in vs {
                    v.hash(state);
                }
            }
            Value::Map(m) => {
                state.write_u8(7);
                m.len().hash(state);
                for (k, v) in m.iter() {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Value::Set(s) => {
                state.write_u8(8);
                s.len().hash(state);
                for v in s.iter() {
                    v.hash(state);
                }
            }
            Value::Record(_, fields) => {
                state.write_u8(9);
                // Do NOT hash the type. `PartialEq` treats an anonymous
                // record as equal to a nominal one with the same fields
                // (the `<anon>` wildcard), so the Hash contract `a == b ⇒
                // hash(a) == hash(b)` requires the same fields to hash to
                // the same value whatever the type. Two distinct nominal
                // types with the same fields (`Person{x:1}` vs
                // `Car{x:1}`) still compare unequal, so they just
                // hash-collide and are told apart by Eq.
                for (k, v) in fields.iter() {
                    k.hash(state);
                    v.hash(state);
                }
            }
            Value::Variant(tag, fields) => {
                state.write_u8(10);
                tag.hash(state);
                fields.len().hash(state);
                for f in fields {
                    f.hash(state);
                }
            }
            Value::Channel(ch) => {
                state.write_u8(11);
                ch.id().hash(state);
            }
            Value::Handle(h) => {
                state.write_u8(12);
                h.id.hash(state);
            }
            // Content-hash bytes — Eq/Hash contract requires the same
            // structural treatment as PartialEq.
            Value::Bytes(b) => {
                state.write_u8(13);
                b.len().hash(state);
                state.write(b.as_slice());
            }
            Value::TcpListener(t) => {
                state.write_u8(14);
                t.id.hash(state);
            }
            Value::TcpStream(t) => {
                state.write_u8(15);
                t.id.hash(state);
            }
            Value::VmClosure(_) => {
                state.write_u8(16);
                // not meaningfully hashable
            }
            Value::BuiltinFn(id) => {
                state.write_u8(17);
                id.hash(state);
            }
            Value::HostFn(h) => {
                state.write_u8(21);
                h.name.hash(state);
            }
            Value::VariantConstructor(tag) => {
                state.write_u8(18);
                tag.hash(state);
            }
            Value::TypeDescriptor(ty) => {
                state.write_u8(19);
                ty.id.hash(state);
            }
            Value::PrimitiveDescriptor(name) => {
                state.write_u8(20);
                name.hash(state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── range_len overflow regression (L2) ──────────────────────────

    /// L2: `range_len(i64::MIN, i64::MAX)` used to overflow at
    /// `hi - lo + 1`. After fix it must saturate to `i64::MAX`.
    #[test]
    fn range_len_no_overflow_on_full_i64_range() {
        assert_eq!(range_len(i64::MIN, i64::MAX), i64::MAX);
        assert_eq!(range_len(0, i64::MAX), i64::MAX);
        assert_eq!(range_len(i64::MIN, 0), i64::MAX);
        assert_eq!(range_len(i64::MIN, -1), i64::MAX);
        // Normal small ranges still work.
        assert_eq!(range_len(1, 5), 5);
        assert_eq!(range_len(0, 0), 1);
        assert_eq!(range_len(10, 5), 0); // empty
    }
}
