use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use super::list::Elements;
use super::{List, Record, Value, Variant};
use crate::typeinfo::{TypeInfo, ty};

/// The order of a list that holds the elements `items` and the list
/// of the Ints from `lo` to `hi`: element by element, and a list that
/// ends first is the lesser.
///
/// Never inlined: comparing a value nested a million levels deep comes
/// through `List`'s `eq` and `cmp` once a level, and with this loop
/// inside them each level kept 112 bytes of the native stack where the
/// comparison of two slices alone keeps none (it is their last call).
/// tests/lang/deep_value_stack_tests.rs runs at that depth.
#[inline(never)]
fn cmp_items_ints(items: &[Value], lo: i64, hi: i64) -> Ordering {
    for (item, n) in items.iter().zip(lo..=hi) {
        let ordering = item.cmp(&Value::Int(n));
        if ordering.is_ne() {
            return ordering;
        }
    }
    (items.len() as u64).cmp(&(hi.abs_diff(lo) + 1))
}

/// Two lists are equal if they have the same elements, however each is
/// stored.
impl PartialEq for List {
    fn eq(&self, other: &List) -> bool {
        match (self.elements(), other.elements()) {
            (Elements::Items(a), Elements::Items(b)) => a == b,
            (Elements::Ints(a_lo, a_hi), Elements::Ints(b_lo, b_hi)) => {
                a_lo == b_lo && a_hi == b_hi
            }
            (Elements::Items(items), Elements::Ints(lo, hi))
            | (Elements::Ints(lo, hi), Elements::Items(items)) => {
                cmp_items_ints(items, lo, hi).is_eq()
            }
        }
    }
}

impl Eq for List {}

impl PartialOrd for List {
    fn partial_cmp(&self, other: &List) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for List {
    fn cmp(&self, other: &List) -> Ordering {
        match (self.elements(), other.elements()) {
            (Elements::Items(a), Elements::Items(b)) => a.cmp(b),
            // Of two runs of Ints the one that begins lower is the
            // lesser, and of two that begin alike the shorter.
            (Elements::Ints(a_lo, a_hi), Elements::Ints(b_lo, b_hi)) => {
                a_lo.cmp(&b_lo).then(a_hi.cmp(&b_hi))
            }
            (Elements::Items(items), Elements::Ints(lo, hi)) => cmp_items_ints(items, lo, hi),
            (Elements::Ints(lo, hi), Elements::Items(items)) => {
                cmp_items_ints(items, lo, hi).reverse()
            }
        }
    }
}

/// Equal lists hash alike however each is stored, and a list that
/// holds no element (`0..4000000000`) is not walked for its hash: a
/// list of Ints that each are one more than the one before is hashed
/// by its length and its first element, whether it holds them or not.
/// (A list that holds them is read once for that, up to the first
/// element that does not fit, and its hash is as much work again.)
impl Hash for List {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.len().hash(state);
        if let Some((lo, _)) = self.ascending_ints() {
            state.write_u8(1);
            lo.hash(state);
            return;
        }
        state.write_u8(0);
        if let Elements::Items(items) = self.elements() {
            for item in items {
                item.hash(state);
            }
        }
    }
}

/// Two variants are equal if they are the same variant of the same
/// type and their fields are equal.
impl PartialEq for Variant {
    fn eq(&self, other: &Variant) -> bool {
        self.ordinal() == other.ordinal()
            && self.type_id() == other.type_id()
            && self.fields() == other.fields()
    }
}

impl Eq for Variant {}

impl PartialOrd for Variant {
    fn partial_cmp(&self, other: &Variant) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Variants of one type order by declaration, then by their fields;
/// variants of two types (which a program cannot compare) by their
/// types' ids.
impl Ord for Variant {
    fn cmp(&self, other: &Variant) -> Ordering {
        match (self.type_id(), self.ordinal()).cmp(&(other.type_id(), other.ordinal())) {
            Ordering::Equal => self.fields().cmp(other.fields()),
            unequal => unequal,
        }
    }
}

/// Whether the type of `record` declares its fields in the order of
/// their names (an anonymous record's type does).
fn in_name_order(record: &Record) -> bool {
    record.ty().fields().is_sorted_by(|(a, _), (b, _)| a <= b)
}

/// Whether records of the type `ty` are ordered by their fields in
/// name order: a builtin record but `Date`, `Time` and `DateTime`,
/// which like a record of a program's type go in the order of their
/// declaration, the largest unit first.
fn ordered_by_name(ty: &TypeInfo) -> bool {
    ty.is_builtin() && !matches!(ty.id, ty::DATE | ty::TIME | ty::DATE_TIME)
}

/// Two records of one declared type are equal if their fields are.
///
/// The typechecker let a nominal record and an anonymous record of the
/// same shape meet (`unify_anon_nominal`) with no change to the value,
/// so when either side is anonymous the fields alone decide, by their
/// names. Two nominal types (`Person{x:1}` vs `Car{x:1}`) are never
/// unified and compare unequal.
impl PartialEq for Record {
    fn eq(&self, other: &Record) -> bool {
        let (a, b) = (self.ty(), other.ty());
        if Arc::ptr_eq(a, b) || (a.id == b.id && !a.is_anon()) {
            return self.fields() == other.fields();
        }
        (a.is_anon() || b.is_anon()) && self.by_name() == other.by_name()
    }
}

impl Eq for Record {}

impl PartialOrd for Record {
    fn partial_cmp(&self, other: &Record) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Records of one declared type order by their fields in the order the
/// type declares them; records of two types by the types' ids.
///
/// Mirror the `<anon>` wildcard of `PartialEq`: when either side is an
/// anonymous record, the typechecker has already decided these are the
/// same type, and Ord must agree so that `a == b ⇒ cmp(a, b) == Equal`;
/// otherwise BTreeSet / BTreeMap would treat equal values as distinct.
/// An anonymous record, which has no declaration, orders by its fields
/// in name order, and so does a builtin record ([`ordered_by_name`]).
impl Ord for Record {
    fn cmp(&self, other: &Record) -> Ordering {
        let (a, b) = (self.ty(), other.ty());
        if Arc::ptr_eq(a, b) && a.is_anon() {
            return self.named().cmp(other.named());
        }
        if a.is_anon() || b.is_anon() {
            return self.by_name().cmp(&other.by_name());
        }
        a.id.cmp(&b.id).then_with(|| match ordered_by_name(a) {
            true => self.by_name().cmp(&other.by_name()),
            false => self.fields().cmp(other.fields()),
        })
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
            (Value::Variant(a), Value::Variant(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Set(a), Value::Set(b)) => a == b,
            (Value::Record(a), Value::Record(b)) => a == b,
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
            (Value::List(a), Value::List(b)) => a.cmp(b),
            (Value::Tuple(a), Value::Tuple(b)) => a.cmp(b),
            (Value::Map(a), Value::Map(b)) => a.iter().cmp(b.iter()),
            (Value::Set(a), Value::Set(b)) => a.iter().cmp(b.iter()),
            (Value::Record(a), Value::Record(b)) => a.cmp(b),
            (Value::Variant(a), Value::Variant(b)) => a.cmp(b),
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
            Value::List(xs) => {
                state.write_u8(5);
                xs.hash(state);
            }
            Value::Tuple(vs) => {
                state.write_u8(6);
                vs.len().hash(state);
                for v in vs.iter() {
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
            Value::Record(record) => {
                state.write_u8(9);
                // Do NOT hash the type. `PartialEq` treats an anonymous
                // record as equal to a nominal one with the same fields
                // (the `<anon>` wildcard), so the Hash contract `a == b ⇒
                // hash(a) == hash(b)` requires the same fields to hash to
                // the same value whatever the type, and so in one
                // order whatever the type's: the names'. Two distinct
                // nominal types with the same fields (`Person{x:1}` vs
                // `Car{x:1}`) still compare unequal, so they just
                // hash-collide and are told apart by Eq.
                let mut field = |(name, value): (&str, &Value)| {
                    name.hash(state);
                    value.hash(state);
                };
                match in_name_order(record) {
                    true => record.named().for_each(&mut field),
                    false => record.by_name().into_iter().for_each(&mut field),
                }
            }
            Value::Variant(variant) => {
                state.write_u8(10);
                variant.type_id().hash(state);
                variant.ordinal().hash(state);
                let fields = variant.fields();
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
