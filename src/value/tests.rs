use super::*;
use crate::typeinfo::{bv, ty};
use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn hash_of(v: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

fn make_date(year: i64, month: i64, day: i64) -> Value {
    Value::builtin_record(
        ty::DATE,
        [
            ("year", Value::Int(year)),
            ("month", Value::Int(month)),
            ("day", Value::Int(day)),
        ],
    )
}

fn make_time(hour: i64, minute: i64, second: i64, ns: i64) -> Value {
    Value::builtin_record(
        ty::TIME,
        [
            ("hour", Value::Int(hour)),
            ("minute", Value::Int(minute)),
            ("second", Value::Int(second)),
            ("ns", Value::Int(ns)),
        ],
    )
}

/// The record type `name` with the id `id` and the `Int` fields
/// `fields`.
fn record_type(id: u32, name: &str, fields: &[&str]) -> Arc<TypeInfo> {
    use crate::defs::{DefId, TypeId};
    use crate::typeinfo::FieldType;
    let fields = fields
        .iter()
        .map(|field| (field.to_string(), FieldType::Int));
    TypeInfo::new_record(TypeId(DefId(id)), name, fields.collect())
}

// ── Hash/Eq consistency ────────────────────────────────────────

/// The Float `f`, a finite number, as a value.
fn float(f: f64) -> Value {
    Value::Float(Float::new(f).expect("a finite number"))
}

/// A Float is finite and there is one zero: `-0.0` is made `0.0`, a NaN
/// and the infinities are no Floats. So a Float is its own key: equal
/// Floats are the same bits and hash alike, and any two are in order.
#[test]
fn a_float_is_finite_and_has_one_zero() {
    let (zero, negative) = (Float::new(0.0), Float::new(-0.0));
    assert_eq!(zero, negative);
    let zero = zero.expect("a Float");
    assert!(zero.get().is_sign_positive());
    assert!(negative.expect("a Float").get().is_sign_positive());
    assert_eq!(hash_of(&float(0.0)), hash_of(&float(-0.0)));
    assert_eq!(format!("{}", float(-0.0)), "0.0");
    for none in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(Float::new(none), None);
    }
    for some in [f64::MAX, f64::MIN, f64::MIN_POSITIVE, -1.5e-300] {
        assert_eq!(Float::new(some).map(Float::get), Some(some));
    }
    // What stays among the Floats: a Float negated, an Int as a Float.
    assert_eq!(-zero, zero);
    assert!((-zero).get().is_sign_positive());
    assert_eq!((-Float::new(1.5).expect("a Float")).get(), -1.5);
    assert_eq!(Float::from(i64::MAX).get(), i64::MAX as f64);
    assert!(Float::from(0).get().is_sign_positive());
    // The order is the numbers'.
    let ascending = [f64::MIN, -2.0, -1e-300, 0.0, 1e-300, 2.0, f64::MAX];
    for pair in ascending.windows(2) {
        assert!(float(pair[0]) < float(pair[1]), "{} < {}", pair[0], pair[1]);
        assert_ne!(hash_of(&float(pair[0])), hash_of(&float(pair[1])));
    }
}

/// A Float's text has a point and no exponent, and a whole Float ends
/// in `.0`.
#[test]
fn a_float_is_written_with_a_point() {
    for (number, text) in [
        (1.0, "1.0"),
        (-3.0, "-3.0"),
        (0.0, "0.0"),
        (2.5, "2.5"),
        (0.1 + 0.2, "0.30000000000000004"),
        (1e21, "1000000000000000000000.0"),
        (-1e21, "-1000000000000000000000.0"),
        (1e-7, "0.0000001"),
        (9007199254740993.0, "9007199254740992.0"),
    ] {
        assert_eq!(format!("{}", float(number)), text);
        assert_eq!(format!("{:?}", float(number)), text);
    }
    let largest = format!("{}", float(f64::MAX));
    assert_eq!(largest.len(), 311);
    assert!(largest.starts_with("17976931348623157") && largest.ends_with("0.0"));
}

/// A width, an alignment, a `+` and a `0` place a Float's text as they
/// place a number's; a precision asks for its own decimals, as of an
/// `f64`.
#[test]
fn a_float_s_text_takes_a_width_and_a_precision() {
    let (whole, part) = (float(3.0), float(-2.5));
    let (Value::Float(whole), Value::Float(part)) = (whole, part) else {
        panic!("two Floats");
    };
    assert_eq!(format!("{whole:.2}"), "3.00");
    assert_eq!(format!("{part:.2}"), "-2.50");
    assert_eq!(format!("{whole:.0}"), "3");
    assert_eq!(format!("{whole:>8}"), "     3.0");
    assert_eq!(format!("{part:>8}"), "    -2.5");
    assert_eq!(format!("{whole:<8}|"), "3.0     |");
    assert_eq!(format!("{part:<8}|"), "-2.5    |");
    assert_eq!(format!("{whole:^7}|"), "  3.0  |");
    assert_eq!(format!("{whole:*>6}"), "***3.0");
    assert_eq!(format!("{whole:08.3}"), "0003.000");
    assert_eq!(format!("{part:08.3}"), "-002.500");
    // Without an alignment a number stands to the right; a `0` pads
    // with zeros behind the sign; a `+` shows the sign of what is not
    // negative.
    assert_eq!(format!("{whole:8}"), "     3.0");
    assert_eq!(format!("{part:8}"), "    -2.5");
    assert_eq!(format!("{part:^8}|"), "  -2.5  |");
    assert_eq!(format!("{whole:08}"), "000003.0");
    assert_eq!(format!("{part:08}"), "-00002.5");
    assert_eq!(format!("{whole:+}"), "+3.0");
    assert_eq!(format!("{part:+}"), "-2.5");
    assert_eq!(format!("{whole:+08}"), "+00003.0");
    // A width the text already fills changes nothing.
    assert_eq!(format!("{whole:2}"), "3.0");
    assert_eq!(format!("{whole:?}"), "3.0");
}

#[test]
fn hash_eq_int_values() {
    let a = Value::Int(42);
    let b = Value::Int(42);
    assert_eq!(a, b);
    assert_eq!(hash_of(&a), hash_of(&b));
}

#[test]
fn hash_eq_string_values() {
    let a = Value::String("hello".into());
    let b = Value::String("hello".into());
    assert_eq!(a, b);
    assert_eq!(hash_of(&a), hash_of(&b));
}

#[test]
fn hash_eq_list_values() {
    let a = Value::list(vec![Value::Int(1), Value::Int(2)]);
    let b = Value::list(vec![Value::Int(1), Value::Int(2)]);
    assert_eq!(a, b);
    assert_eq!(hash_of(&a), hash_of(&b));
}

#[test]
fn hash_eq_record_values() {
    let a = make_date(2025, 1, 15);
    let b = make_date(2025, 1, 15);
    assert_eq!(a, b);
    assert_eq!(hash_of(&a), hash_of(&b));
}

#[test]
fn hash_eq_variant_values() {
    let a = Value::variant(bv::OK, vec![Value::Int(42)]);
    let b = Value::variant(bv::OK, vec![Value::Int(42)]);
    assert_eq!(a, b);
    assert_eq!(hash_of(&a), hash_of(&b));
}

// ── PartialEq edge cases ───────────────────────────────────────

#[test]
fn empty_list_eq() {
    let a = Value::list(vec![]);
    let b = Value::list(vec![]);
    assert_eq!(a, b);
}

#[test]
fn nested_list_eq() {
    let inner1 = Value::list(vec![Value::Int(1)]);
    let inner2 = Value::list(vec![Value::Int(1)]);
    let a = Value::list(vec![inner1]);
    let b = Value::list(vec![inner2]);
    assert_eq!(a, b);
}

#[test]
fn different_variant_types_not_equal() {
    assert_ne!(Value::Int(1), float(1.0));
    assert_ne!(Value::Int(0), Value::Bool(false));
    assert_ne!(Value::String("1".into()), Value::Int(1));
}

#[test]
fn unit_eq() {
    assert_eq!(Value::Unit, Value::Unit);
}

#[test]
fn tuple_eq() {
    let a = Value::tuple(vec![Value::Int(1), Value::String("x".into())]);
    let b = Value::tuple(vec![Value::Int(1), Value::String("x".into())]);
    assert_eq!(a, b);
}

#[test]
fn tuple_neq_different_lengths() {
    let a = Value::tuple(vec![Value::Int(1)]);
    let b = Value::tuple(vec![Value::Int(1), Value::Int(2)]);
    assert_ne!(a, b);
}

// ── Ord correctness ────────────────────────────────────────────

#[test]
fn ord_int_ordering() {
    assert!(Value::Int(1) < Value::Int(2));
    assert!(Value::Int(-5) < Value::Int(0));
    assert_eq!(Value::Int(42).cmp(&Value::Int(42)), Ordering::Equal);
}

#[test]
fn ord_string_ordering() {
    assert!(Value::String("apple".into()) < Value::String("banana".into()));
    assert!(Value::String("a".into()) < Value::String("b".into()));
}

#[test]
fn ord_date_records() {
    let earlier = make_date(2024, 6, 15);
    let later = make_date(2024, 7, 1);
    let same_year_month = make_date(2024, 6, 20);
    assert!(earlier < later, "June 15 < July 1");
    assert!(earlier < same_year_month, "June 15 < June 20");
    assert_eq!(
        make_date(2024, 6, 15).cmp(&make_date(2024, 6, 15)),
        Ordering::Equal
    );
}

#[test]
fn ord_date_year_takes_priority() {
    let d2023 = make_date(2023, 12, 31);
    let d2024 = make_date(2024, 1, 1);
    assert!(d2023 < d2024, "2023-12-31 < 2024-01-01");
}

#[test]
fn ord_time_records() {
    let earlier = make_time(10, 30, 0, 0);
    let later = make_time(10, 31, 0, 0);
    assert!(earlier < later, "10:30:00 < 10:31:00");
    let by_hour = make_time(9, 59, 59, 0);
    assert!(by_hour < earlier, "09:59:59 < 10:30:00");
}

#[test]
fn ord_time_ns_tiebreaker() {
    let a = make_time(12, 0, 0, 100);
    let b = make_time(12, 0, 0, 200);
    assert!(a < b, "ns should break ties in time ordering");
}

#[test]
fn ord_weekday_variants() {
    let monday = Value::variant(bv::MONDAY, vec![]);
    let tuesday = Value::variant(bv::TUESDAY, vec![]);
    let friday = Value::variant(bv::FRIDAY, vec![]);
    let sunday = Value::variant(bv::SUNDAY, vec![]);
    assert!(monday < tuesday, "Monday < Tuesday");
    assert!(tuesday < friday, "Tuesday < Friday");
    assert!(friday < sunday, "Friday < Sunday");
    assert_eq!(
        Value::variant(bv::WEDNESDAY, vec![]).cmp(&Value::variant(bv::WEDNESDAY, vec![])),
        Ordering::Equal,
    );
}

#[test]
fn ord_result_variants_decl_order() {
    // Result is declared as `type Result(a, e) { Ok(a), Err(e) }`,
    // so Ok has ordinal 0, Err ordinal 1, and `Ok < Err`.
    let ok = Value::variant(bv::OK, vec![Value::Int(1)]);
    let err = Value::variant(bv::ERR, vec![Value::String("e".into())]);
    assert!(ok < err, "Ok declared before Err → Ok < Err");
}

/// Two enums with variants of one name each order by their own
/// declaration, and their variants are not equal.
#[test]
fn variants_of_one_name_in_two_enums_stay_apart() {
    use crate::defs::{DefId, TypeId};
    let a = TypeInfo::new_enum(TypeId(DefId(9000)), "A", &[("Red", 0), ("Blue", 0)]);
    let b = TypeInfo::new_enum(TypeId(DefId(9001)), "B", &[("Blue", 0), ("Red", 0)]);
    let value = |ty: &Arc<TypeInfo>, name: &str| {
        Value::variant(Tag::named(ty, name).expect("a variant"), vec![])
    };
    assert!(value(&a, "Red") < value(&a, "Blue"));
    assert!(value(&b, "Blue") < value(&b, "Red"));
    assert_ne!(value(&a, "Red"), value(&b, "Red"));
    assert_eq!(hash_of(&value(&a, "Red")), hash_of(&value(&a, "Red")));
}

/// The hash of a variant is its kind, which variant of its type it is,
/// and its fields with their number, and nothing of the type: the same
/// when the type has
/// another id (another type is declared before it), and the hash this
/// test writes out by hand.
#[test]
fn the_hash_of_a_variant_holds_nothing_of_its_type() {
    use crate::defs::{DefId, TypeId};
    let shape = |id: u32, name: &str| {
        TypeInfo::new_enum(TypeId(DefId(id)), name, &[("Dot", 0), ("Box", 2)])
    };
    let boxed = |ty: &Arc<TypeInfo>| {
        let tag = Tag::named(ty, "Box").expect("a variant");
        Value::variant(tag, vec![Value::Int(3), Value::Int(4)])
    };
    let (here, there) = (shape(9010, "Shape"), shape(9011, "Other"));
    assert_eq!(hash_of(&boxed(&here)), hash_of(&boxed(&there)));
    let mut by_hand = DefaultHasher::new();
    // The variant: its rank, its place in the type and the number of
    // its fields. The two Ints.
    by_hand.write_u8(10);
    1u16.hash(&mut by_hand);
    2usize.hash(&mut by_hand);
    for field in [3i64, 4] {
        by_hand.write_u8(2);
        field.hash(&mut by_hand);
    }
    assert_eq!(hash_of(&boxed(&here)), by_hand.finish());
}

/// A variant without fields is its type, counted once more: nothing
/// is made for it. A variant with fields is made once, and a clone of
/// it has the same fields, not a copy of them.
#[test]
fn a_variant_is_shared_not_copied() {
    use crate::defs::{DefId, TypeId};
    let ty = TypeInfo::new_enum(TypeId(DefId(9003)), "Chain", &[("End", 0), ("Link", 2)]);
    let tag = |name: &str| Tag::named(&ty, name).expect("a variant");
    let held = Arc::strong_count(&ty);

    let end = Value::variant(tag("End"), vec![]);
    let ends = vec![end.clone(); 3];
    assert_eq!(Arc::strong_count(&ty), held + 4);
    let Value::Variant(variant) = &end else {
        panic!("a variant");
    };
    assert_eq!((variant.name(), variant.ordinal()), ("End", 0));
    assert!(variant.fields().is_empty());
    assert!(Arc::ptr_eq(variant.ty(), &ty));
    drop(ends);

    let link = Value::variant(tag("Link"), vec![Value::Int(1), end.clone()]);
    let copy = link.clone();
    let (Value::Variant(first), Value::Variant(second)) = (&link, &copy) else {
        panic!("two variants");
    };
    assert_eq!((first.name(), first.ordinal()), ("Link", 1));
    assert_eq!(first.fields(), [Value::Int(1), end.clone()]);
    assert!(std::ptr::eq(first.fields(), second.fields()));
    assert!(Arc::ptr_eq(first.ty(), &ty) && first.has_tag(&tag("Link")));
    assert_eq!(link, copy);
    assert_eq!(hash_of(&link), hash_of(&copy));

    drop((end, link, copy));
    assert_eq!(Arc::strong_count(&ty), held);
}

/// A clone of a string or of a tuple is the same text and the same
/// items, counted once more.
#[test]
fn a_string_and_a_tuple_are_shared_not_copied() {
    let text = Value::String("silt".repeat(1000).into());
    let tuple = Value::tuple(vec![text.clone(), Value::Int(1)]);
    let (text_again, tuple_again) = (text.clone(), tuple.clone());
    let (Value::String(a), Value::String(b)) = (&text, &text_again) else {
        panic!("two strings");
    };
    assert!(Arc::ptr_eq(a, b));
    assert_eq!(Arc::strong_count(a), 3, "the two names' and the tuple's");
    let (Value::Tuple(a), Value::Tuple(b)) = (&tuple, &tuple_again) else {
        panic!("two tuples");
    };
    assert!(std::ptr::eq(a.as_ptr(), b.as_ptr()));
    assert_eq!(a[..], [text.clone(), Value::Int(1)]);
}

/// A record holds its fields in the order its type declares them,
/// however a literal writes them; a field is found by its name, and a
/// clone of the record has the same fields, not a copy of them.
#[test]
fn a_record_holds_its_fields_in_declaration_order() {
    let ty = record_type(9004, "Ver", &["major", "build", "label"]);
    let values = vec![Value::Int(7), Value::Int(1), Value::Int(9)];
    let written = Record::written(ty.clone(), ["label", "major", "build"].into_iter(), values)
        .expect("the type's fields");
    assert_eq!(
        written.fields(),
        [Value::Int(1), Value::Int(9), Value::Int(7)]
    );
    let names: Vec<&str> = written.named().map(|(name, _)| name).collect();
    assert_eq!(names, ["major", "build", "label"]);
    assert_eq!(written.get("build"), Some(&Value::Int(9)));
    assert_eq!(written.get("patch"), None);
    assert!(Arc::ptr_eq(written.ty(), &ty));
    let record = Value::Record(written.clone());
    assert_eq!(
        record,
        Value::record(
            ty.clone(),
            vec![Value::Int(1), Value::Int(9), Value::Int(7)]
        )
    );
    assert_eq!(format!("{record}"), "Ver {major: 1, build: 9, label: 7}");
    assert!(std::ptr::eq(written.clone().fields(), written.fields()));

    // Not the type's fields: one missing, one too many, one twice.
    let two = || vec![Value::Int(1), Value::Int(2)];
    let three = || vec![Value::Int(1), Value::Int(2), Value::Int(3)];
    assert!(Record::written(ty.clone(), ["major", "build"].into_iter(), two()).is_none());
    assert!(
        Record::written(ty.clone(), ["major", "build", "patch"].into_iter(), three()).is_none()
    );
    assert!(
        Record::written(ty.clone(), ["major", "build", "build"].into_iter(), three()).is_none()
    );

    // Order is the declaration's: `major` first, whatever the names'.
    let ver = |major, build| {
        Value::record(
            ty.clone(),
            vec![Value::Int(major), Value::Int(build), Value::Int(0)],
        )
    };
    assert!(ver(1, 9) < ver(2, 0));
}

/// An update of fields a record has replaces them and keeps the type;
/// the one holder of a record updates it where it is.
#[test]
fn a_record_update_replaces_fields_by_name() {
    let ty = record_type(9005, "Pt", &["y", "x"]);
    let record = Record::written(
        ty.clone(),
        ["y", "x"].into_iter(),
        vec![Value::Int(1), Value::Int(2)],
    )
    .expect("the type's fields");
    let kept = record.clone();
    let updated = record
        .updated(["x"].into_iter(), vec![Value::Int(5)])
        .expect("a field of the record");
    assert_eq!(updated.fields(), [Value::Int(1), Value::Int(5)]);
    assert_eq!(kept.fields(), [Value::Int(1), Value::Int(2)]);
    assert!(Arc::ptr_eq(updated.ty(), &ty));

    let at = updated.fields().as_ptr();
    let again = updated
        .updated(["y", "x"].into_iter(), vec![Value::Int(8), Value::Int(9)])
        .expect("fields of the record");
    assert_eq!(again.fields(), [Value::Int(8), Value::Int(9)]);
    assert_eq!(
        again.fields().as_ptr(),
        at,
        "no other holder: updated in place"
    );

    // A declared record has the fields of its type and no other.
    assert!(
        again
            .updated(["z"].into_iter(), vec![Value::Int(0)])
            .is_none()
    );
}

/// Anonymous records of one set of field names have one type, whose
/// fields are in name order; a field added to one makes a record of
/// the larger set, and the rest of a record is an anonymous record.
#[test]
fn anonymous_records_of_one_set_of_names_share_a_type() {
    let fields = |record: &Value| -> Vec<(String, Value)> {
        let Value::Record(record) = record else {
            panic!("a record");
        };
        let named = record.named();
        named
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect()
    };
    let ty = |record: &Value| match record {
        Value::Record(record) => record.ty().clone(),
        _ => panic!("a record"),
    };
    let a = Value::anon_record([("y", Value::Int(2)), ("x", Value::Int(1))]);
    let b = Value::anon_record([("x", Value::Int(1)), ("y", Value::Int(2))]);
    assert!(Arc::ptr_eq(&ty(&a), &ty(&b)));
    assert!(ty(&a).is_anon());
    assert_eq!(
        fields(&a),
        [
            ("x".to_string(), Value::Int(1)),
            ("y".to_string(), Value::Int(2))
        ]
    );
    assert_eq!(a, b);
    assert_eq!(hash_of(&a), hash_of(&b));
    assert_eq!(format!("{a}"), "{x: 1, y: 2}");

    let other = Value::anon_record([("x", Value::Int(1)), ("z", Value::Int(2))]);
    assert!(!Arc::ptr_eq(&ty(&a), &ty(&other)));
    assert_ne!(a, other);
    let none = Value::anon_record([]);
    assert!(Arc::ptr_eq(
        &ty(&none),
        crate::typeinfo::builtin_type(ty::ANON_RECORD)
    ));
    assert_eq!(format!("{none}"), "{}");

    // A spread's written fields: one the record has is replaced, one
    // it has not is added.
    let (Value::Record(base), Value::Record(wider)) = (&a, &other) else {
        panic!("two records");
    };
    let replaced = base
        .clone()
        .updated(["y"].into_iter(), vec![Value::Int(7)])
        .expect("an anonymous record");
    assert!(Arc::ptr_eq(replaced.ty(), base.ty()));
    assert_eq!(replaced.fields(), [Value::Int(1), Value::Int(7)]);
    let added = base
        .clone()
        .updated(["z", "x"].into_iter(), vec![Value::Int(2), Value::Int(1)])
        .expect("an anonymous record");
    let added = Value::Record(added);
    assert_eq!(format!("{added}"), "{x: 1, y: 2, z: 2}");
    assert_eq!(base.fields(), [Value::Int(1), Value::Int(2)]);

    // The rest of a record: of an anonymous record with nothing left
    // out, the record; of a declared record, an anonymous record.
    assert!(std::ptr::eq(
        base.rest([].into_iter()).fields(),
        base.fields()
    ));
    assert_eq!(
        Value::Record(wider.rest(["x"].into_iter())),
        Value::anon_record([("z", Value::Int(2))])
    );
    let pt = Value::record(
        record_type(9006, "Pt", &["y", "x"]),
        vec![Value::Int(2), Value::Int(1)],
    );
    let Value::Record(declared) = &pt else {
        panic!("a record");
    };
    let rest = Value::Record(declared.rest([].into_iter()));
    assert!(ty(&rest).is_anon());
    assert_eq!(format!("{pt}"), "Pt {y: 2, x: 1}");
    assert_eq!(format!("{rest}"), "{x: 1, y: 2}");
    assert_eq!(rest, a);
}

/// A builtin record is built with its fields named, in the order its
/// type declares them, and is written and ordered in that order.
#[test]
fn a_builtin_record_has_the_fields_its_type_declares() {
    let date = make_date(2024, 3, 9);
    let Value::Record(record) = &date else {
        panic!("a record");
    };
    assert_eq!(
        record.fields(),
        [Value::Int(2024), Value::Int(3), Value::Int(9)]
    );
    assert_eq!(record.get("month"), Some(&Value::Int(3)));
    assert_eq!(date.format_silt(), "Date {year: 2024, month: 3, day: 9}");
    assert_eq!(date.to_string(), "2024-03-09");
    assert!(make_date(2024, 3, 9) < make_date(2024, 10, 1));
}

/// A record is of its type. Records of two declared types with the
/// same fields are two values, and an anonymous record is not a
/// declared one of the same fields: not equal, in the order of their
/// types, and two members of a set. (An anonymous record once stood
/// for any record of its fields, in `==`, in the order and in the
/// hash: what a set held depended on the order its members came in.)
#[test]
fn a_record_is_of_its_type() {
    let person = Value::record(record_type(9100, "Person", &["x"]), vec![Value::Int(1)]);
    let again = Value::record(record_type(9100, "Person", &["x"]), vec![Value::Int(1)]);
    let car = Value::record(record_type(9101, "Car", &["x"]), vec![Value::Int(1)]);
    let anon = Value::anon_record([("x", Value::Int(1))]);

    // One type, described twice: one value.
    assert_eq!(person, again);
    assert_eq!(person.cmp(&again), Ordering::Equal);
    assert_eq!(hash_of(&person), hash_of(&again));

    for (a, b) in [(&person, &car), (&person, &anon), (&car, &anon)] {
        assert_ne!(a, b);
        assert_ne!(a.cmp(b), Ordering::Equal);
        assert_eq!(a.cmp(b), b.cmp(a).reverse());
    }
    for order in [
        [&person, &car, &anon],
        [&anon, &car, &person],
        [&car, &anon, &person],
    ] {
        let members: std::collections::BTreeSet<Value> = order.into_iter().cloned().collect();
        assert_eq!(members.len(), 3);
        let hashed: std::collections::HashSet<Value> = order.into_iter().cloned().collect();
        assert_eq!(hashed.len(), 3);
    }

    // Anonymous records of other fields are other values, and each is
    // itself.
    let wider = Value::anon_record([("x", Value::Int(1)), ("y", Value::Int(2))]);
    let named = Value::anon_record([("z", Value::Int(1))]);
    assert_ne!(anon, wider);
    assert_ne!(anon, named);
    assert_ne!(anon.cmp(&wider), Ordering::Equal);
    assert_ne!(anon.cmp(&named), Ordering::Equal);
    assert_eq!(anon, Value::anon_record([("x", Value::Int(1))]));
    assert_ne!(anon, Value::anon_record([("x", Value::Int(2))]));
    assert!(anon < Value::anon_record([("x", Value::Int(2))]));
}

/// Records of one type are in the order of their fields as the type
/// declares them, a builtin record like any other; and a record's hash
/// goes through its fields in that order.
#[test]
fn records_are_ordered_and_hashed_in_declaration_order() {
    let ty = record_type(9102, "Ver", &["major", "build"]);
    let ver = |major, build| Value::record(ty.clone(), vec![Value::Int(major), Value::Int(build)]);
    assert!(ver(1, 9) < ver(2, 0));
    assert!(ver(2, 0) < ver(2, 1));
    assert_eq!(ver(2, 1).cmp(&ver(2, 1)), Ordering::Equal);
    assert_eq!(hash_of(&ver(2, 1)), hash_of(&ver(2, 1)));
    assert_ne!(hash_of(&ver(2, 1)), hash_of(&ver(1, 2)));

    // `Response` declares `status` before `body`: the status decides.
    let response = |status, body: &str| {
        let headers = Value::map([]);
        let body = Value::Bytes(Arc::new(body.as_bytes().to_vec()));
        Value::builtin_record(
            ty::RESPONSE,
            [
                ("status", Value::Int(status)),
                ("body", body),
                ("headers", headers),
            ],
        )
    };
    assert!(response(200, "z") < response(404, "a"));
    assert!(response(200, "a") < response(200, "b"));
    assert!(make_date(2024, 3, 9) < make_date(2024, 10, 1));
    assert!(make_time(7, 59, 59, 0) < make_time(8, 0, 0, 0));
}

/// Values of two kinds are in the order of the kinds, one table for
/// the order and for the first byte of the hash; values of one kind
/// that are equal are in no order and hash alike.
#[test]
fn values_of_every_kind_have_one_key() {
    let none = Value::variant(bv::NONE, vec![]);
    let ascending = [
        Value::Unit,
        Value::Bool(false),
        Value::Bool(true),
        Value::Int(-1),
        Value::Int(7),
        float(-0.5),
        float(7.0),
        Value::String("a".into()),
        Value::String("b".into()),
        Value::list(vec![]),
        Value::list(vec![Value::Int(1)]),
        Value::list(vec![Value::Int(1), Value::Int(0)]),
        Value::list(vec![Value::Int(2)]),
        Value::tuple(vec![Value::Int(1), Value::Int(2)]),
        Value::tuple(vec![Value::Int(1), Value::Int(3)]),
        Value::map([(Value::Int(1), Value::Int(1))]),
        Value::map([(Value::Int(1), Value::Int(2))]),
        Value::set([Value::Int(1)]),
        Value::set([Value::Int(1), Value::Int(2)]),
        Value::anon_record([("x", Value::Int(1))]),
        Value::variant(bv::OK, vec![Value::Int(1)]),
        Value::variant(bv::OK, vec![Value::Int(2)]),
        Value::variant(bv::ERR, vec![Value::Int(0)]),
        Value::variant(bv::SOME, vec![none.clone()]),
        none,
        Value::Bytes(Arc::new(vec![1, 2])),
        Value::Bytes(Arc::new(vec![1, 3])),
    ];
    for (at, a) in ascending.iter().enumerate() {
        assert_eq!(a, &a.clone());
        assert_eq!(a.cmp(&a.clone()), Ordering::Equal);
        assert_eq!(hash_of(a), hash_of(&a.clone()));
        for b in &ascending[at + 1..] {
            assert_eq!(a.cmp(b), Ordering::Less, "{a:?} < {b:?}");
            assert_eq!(b.cmp(a), Ordering::Greater, "{b:?} > {a:?}");
            assert_ne!(a, b);
            assert_ne!(hash_of(a), hash_of(b), "{a:?} and {b:?}");
        }
    }
}

/// The key of a value nested 50,000 levels deep is found like any
/// other's: the walk keeps what it has yet to visit in a stack of its
/// own, and a chain in which each value is the last part of the one
/// before waits for nothing. (The comparison recursed once a level:
/// a thread of a test has a stack for a few thousand levels of it.
/// The goldens under lang/values go to a million levels.)
#[test]
fn a_deep_value_is_compared_and_hashed_without_the_native_stack() {
    const DEPTH: i64 = 50_000;
    let node = record_type(9103, "Node", &["tag", "next"]);
    // The next level is the last part of a level (of a record in a
    // list in a variant), where the walk waits for nothing; and its
    // first part (of a tuple), where the walk comes back to each
    // level.
    let last = |n: i64, rest: Value| {
        let record = Value::record(node.clone(), vec![Value::Int(n), rest]);
        Value::variant(bv::OK, vec![Value::list(vec![record])])
    };
    let first = |n: i64, rest: Value| Value::tuple(vec![rest, Value::Int(n)]);
    let shapes: [&dyn Fn(i64, Value) -> Value; 2] = [&last, &first];
    for (shape, wrap) in shapes.into_iter().enumerate() {
        let build =
            |innermost: i64| (0..DEPTH).fold(Value::Int(innermost), |rest, n| wrap(n, rest));
        let (a, b, c) = (build(0), build(0), build(1));
        assert!(a == b, "shape {shape}");
        assert!(a != c, "shape {shape}");
        assert_eq!(a.cmp(&b), Ordering::Equal, "shape {shape}");
        assert_eq!(a.cmp(&c), Ordering::Less, "shape {shape}");
        assert_eq!(hash_of(&a), hash_of(&b), "shape {shape}");
        assert_ne!(hash_of(&a), hash_of(&c), "shape {shape}");
    }
}

/// The walk goes down the first levels of a value by calling itself
/// and the levels below with a stack of its own: what it finds does
/// not depend on which. The hash of a value nested to any depth is the
/// hash this test writes out by hand, and two values that differ at
/// some depth are in the order of what differs there.
#[test]
fn the_key_of_a_value_does_not_depend_on_its_depth() {
    use std::hash::Hasher;
    // A value `depth` levels deep: a tuple of an Int, the value one
    // level in (or the innermost Int) and a string.
    fn nested(depth: usize, innermost: i64) -> Value {
        (0..depth).fold(Value::Int(innermost), |inner, level| {
            Value::tuple(vec![
                Value::Int(level as i64),
                inner,
                Value::String("s".into()),
            ])
        })
    }
    // Its hash, written out: for a tuple its rank, the number of its
    // items and the items; for an Int and a string its rank and it.
    fn by_hand(depth: usize, innermost: i64, state: &mut DefaultHasher) {
        match depth.checked_sub(1) {
            None => {
                state.write_u8(2);
                innermost.hash(state);
            }
            Some(inner) => {
                state.write_u8(6);
                3usize.hash(state);
                state.write_u8(2);
                (inner as i64).hash(state);
                by_hand(inner, innermost, state);
                state.write_u8(4);
                "s".hash(state);
            }
        }
    }
    let near = super::key::NEAR;
    let depths = [
        0,
        1,
        2,
        near - 2,
        near - 1,
        near,
        near + 1,
        near + 2,
        2 * near + 1,
    ];
    for depth in depths {
        let value = nested(depth, 7);
        let mut state = DefaultHasher::new();
        by_hand(depth, 7, &mut state);
        assert_eq!(hash_of(&value), state.finish(), "depth {depth}");
        assert_eq!(value, nested(depth, 7), "depth {depth}");
        assert_eq!(value.cmp(&nested(depth, 7)), Ordering::Equal);
        assert_ne!(value, nested(depth, 8), "depth {depth}");
        assert_eq!(
            value.cmp(&nested(depth, 8)),
            Ordering::Less,
            "depth {depth}"
        );
        assert_eq!(nested(depth, 8).cmp(&value), Ordering::Greater);
    }
}

/// Where the walk stops calling itself and goes on with a stack of its
/// own (`key::NEAR` levels down) is nothing a value's key shows. Two
/// values that differ at one level only, a level around that one, in
/// the first part of the level, in its last part, or in the leaf of a
/// value that deep, are equal, ordered and hashed as a plain recursion
/// over the two says, which this test has as its reference: for
/// tuples, lists, maps (the next level a value between a first and a
/// last entry, and a key), sets (the next level between a first and a
/// last member), variants and records, and for all of them in turn.
/// A map and a set are walked from the frame of a call above that
/// level and from a boxed form on the walk's stack below it.
#[test]
fn the_key_is_the_same_where_the_walk_changes_its_stack() {
    use crate::defs::{DefId, TypeId};
    use std::hash::Hasher;
    let near = super::key::NEAR;
    let record = record_type(9301, "Level", &["a", "n", "z"]);
    let wrapped = TypeInfo::new_enum(
        TypeId(DefId(9302)),
        "Wrapped",
        &[("Leaf", 0), ("W", 3), ("Last", 1)],
    );
    let w = Tag::named(&wrapped, "W").expect("a variant");
    let last = Tag::named(&wrapped, "Last").expect("a variant");
    // A level around `inner`, with `pre` before it and `post` after.
    let shapes: [&dyn Fn(Value, i64, i64) -> Value; 7] = [
        &|inner, pre, post| Value::tuple(vec![Value::Int(pre), inner, Value::Int(post)]),
        // (Not three Ints each one more than the one before: such a
        // list is hashed by its ends.)
        &|inner, pre, post| Value::list(vec![Value::Int(10 + pre), inner, Value::Int(20 + post)]),
        &|inner, pre, post| {
            Value::map([
                (Value::Int(1), Value::Int(pre)),
                (Value::Int(2), inner),
                (Value::Int(3), Value::Int(post)),
            ])
        },
        &|inner, pre, post| {
            Value::map([(Value::Int(pre), Value::Int(0)), (inner, Value::Int(post))])
        },
        // (An Int is the first member of a set, before one that has
        // parts, and a variant the last, after a set.)
        &|inner, pre, post| {
            let after = Value::variant(last.clone(), vec![Value::Int(post)]);
            Value::set([Value::Int(pre), inner, after])
        },
        &|inner, pre, post| {
            Value::variant(w.clone(), vec![Value::Int(pre), inner, Value::Int(post)])
        },
        &|inner, pre, post| {
            Value::record(
                record.clone(),
                vec![Value::Int(pre), inner, Value::Int(post)],
            )
        },
    ];

    // The reference: the rank of a kind, and the order, the equality
    // and the hash of values by a function that calls itself.
    fn rank(value: &Value) -> u8 {
        match value {
            Value::Int(_) => 2,
            Value::List(_) => 5,
            Value::Tuple(_) => 6,
            Value::Map(_) => 7,
            Value::Set(_) => 8,
            Value::Record(_) => 9,
            Value::Variant(_) => 10,
            other => panic!("no value of this test: {other:?}"),
        }
    }
    // The parts of a value, in the order of the walk.
    fn parts(value: &Value) -> Vec<Value> {
        match value {
            Value::Int(_) => Vec::new(),
            Value::List(list) => list.iter().collect(),
            Value::Tuple(items) => items.to_vec(),
            Value::Map(entries) => entries
                .iter()
                .flat_map(|(key, value)| [key.clone(), value.clone()])
                .collect(),
            Value::Set(members) => members.iter().cloned().collect(),
            Value::Record(record) => record.fields().to_vec(),
            Value::Variant(variant) => variant.fields().to_vec(),
            other => panic!("no value of this test: {other:?}"),
        }
    }
    fn order(a: &Value, b: &Value) -> Ordering {
        let heads = match (a, b) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Variant(a), Value::Variant(b)) => a.ordinal().cmp(&b.ordinal()),
            _ => rank(a).cmp(&rank(b)),
        };
        if heads.is_ne() {
            return heads;
        }
        let (a, b) = (parts(a), parts(b));
        for (a, b) in a.iter().zip(&b) {
            let unequal = order(a, b);
            if unequal.is_ne() {
                return unequal;
            }
        }
        a.len().cmp(&b.len())
    }
    fn hashed(value: &Value, state: &mut DefaultHasher) {
        state.write_u8(rank(value));
        let parts = parts(value);
        match value {
            Value::Int(n) => n.hash(state),
            Value::List(_) => {
                parts.len().hash(state);
                state.write_u8(0);
            }
            Value::Tuple(_) | Value::Set(_) => parts.len().hash(state),
            Value::Map(entries) => entries.len().hash(state),
            Value::Variant(variant) => {
                variant.ordinal().hash(state);
                parts.len().hash(state);
            }
            _ => {}
        }
        let names: Vec<&str> = match value {
            Value::Record(record) => record.named().map(|(name, _)| name).collect(),
            _ => Vec::new(),
        };
        for (at, part) in parts.iter().enumerate() {
            if let Some(name) = names.get(at) {
                name.hash(state);
            }
            hashed(part, state);
        }
    }
    let reference_hash = |value: &Value| {
        let mut state = DefaultHasher::new();
        hashed(value, &mut state);
        state.finish()
    };

    // The value `depth` levels deep whose level `l` (the outermost is
    // 1) is made by the shape `shape(l)`, with the leaf `leaf` and, at
    // the level `differs`, `pre` and `post`.
    let build = |shape: &dyn Fn(usize) -> usize, depth, leaf, differs, pre, post| {
        (1..=depth).rev().fold(Value::Int(leaf), |inner, l| {
            let (pre, post) = match l == differs {
                true => (pre, post),
                false => (0, 0),
            };
            shapes[shape(l)](inner, pre, post)
        })
    };
    let deep = near + 4;
    let mut compared = 0;
    for kind in 0..=shapes.len() {
        // Each shape at every level, and last the shapes in turn.
        let shape = move |l: usize| match kind == shapes.len() {
            true => l % shapes.len(),
            false => kind,
        };
        for level in near - 2..=near + 3 {
            let pairs = [
                // The first part of the level, its last part, and the
                // leaf of a value that deep.
                (
                    build(&shape, deep, 0, 0, 0, 0),
                    build(&shape, deep, 0, level, 1, 0),
                ),
                (
                    build(&shape, deep, 0, 0, 0, 0),
                    build(&shape, deep, 0, level, 0, 1),
                ),
                (
                    build(&shape, level, 0, 0, 0, 0),
                    build(&shape, level, 1, 0, 0, 0),
                ),
            ];
            for (which, (same, other)) in pairs.into_iter().enumerate() {
                let what = format!("shape {kind}, level {level}, difference {which}");
                let again = same.clone();
                assert!(same == again && same.cmp(&again).is_eq(), "{what}");
                assert_eq!(order(&same, &again), Ordering::Equal, "{what}");
                let expected = order(&same, &other);
                assert!(expected.is_ne(), "{what}: the reference tells them apart");
                assert!(same != other, "{what}");
                assert_eq!(same.cmp(&other), expected, "{what}");
                assert_eq!(other.cmp(&same), expected.reverse(), "{what}");
                assert_eq!(same < other, expected.is_lt(), "{what}");
                assert_eq!(hash_of(&same), reference_hash(&same), "{what}");
                assert_eq!(hash_of(&other), reference_hash(&other), "{what}");
                assert_ne!(hash_of(&same), hash_of(&other), "{what}");
                compared += 1;
            }
        }
    }
    assert_eq!(compared, (shapes.len() + 1) * 6 * 3);
}

/// A program's record type named like a builtin one prints as a
/// record: Display is keyed by the builtin type's id, not its name.
#[test]
fn a_program_type_named_time_is_not_the_builtin_time() {
    let rec = Value::record(record_type(9002, "Time", &["h"]), vec![Value::Int(1)]);
    assert_eq!(format!("{rec}"), "Time {h: 1}");
}

#[test]
fn ord_cross_type_by_discriminant() {
    assert!(Value::Unit < Value::Bool(true));
    assert!(Value::Bool(false) < Value::Int(0));
    assert!(Value::Int(0) < float(0.0));
}

// ── Display formatting ─────────────────────────────────────────

#[test]
fn display_int() {
    assert_eq!(format!("{}", Value::Int(42)), "42");
    assert_eq!(format!("{}", Value::Int(-1)), "-1");
}

#[test]
fn display_float() {
    assert_eq!(format!("{}", float(4.25)), "4.25");
}

#[test]
fn display_bool() {
    assert_eq!(format!("{}", Value::Bool(true)), "true");
    assert_eq!(format!("{}", Value::Bool(false)), "false");
}

#[test]
fn display_string() {
    assert_eq!(format!("{}", Value::String("hello".into())), "hello");
}

#[test]
fn display_unit() {
    assert_eq!(format!("{}", Value::Unit), "()");
}

#[test]
fn display_list() {
    let list = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    assert_eq!(format!("{}", list), "[1, 2, 3]");
}

#[test]
fn display_empty_list() {
    let list = Value::list(vec![]);
    assert_eq!(format!("{}", list), "[]");
}

#[test]
fn display_tuple() {
    let tuple = Value::tuple(vec![Value::Int(1), Value::String("x".into())]);
    assert_eq!(format!("{}", tuple), "(1, x)");
}

#[test]
fn display_variant_no_fields() {
    assert_eq!(format!("{}", Value::variant(bv::NONE, vec![])), "None");
}

#[test]
fn display_variant_with_fields() {
    let v = Value::variant(bv::SOME, vec![Value::Int(42)]);
    assert_eq!(format!("{}", v), "Some(42)");
}

#[test]
fn display_date_record() {
    assert_eq!(format!("{}", make_date(2024, 3, 5)), "2024-03-05");
}

#[test]
fn display_time_record() {
    assert_eq!(format!("{}", make_time(9, 5, 0, 0)), "09:05:00");
}

#[test]
fn display_time_record_with_ns() {
    assert_eq!(
        format!("{}", make_time(14, 30, 0, 123000000)),
        "14:30:00.123000000"
    );
}

#[test]
fn display_generic_record() {
    let ty = record_type(9003, "Point", &["x", "y"]);
    let rec = Value::record(ty, vec![Value::Int(10), Value::Int(20)]);
    assert_eq!(format!("{}", rec), "Point {x: 10, y: 20}");
}

/// One formatter writes a value in two ways: as it is shown, and as it
/// is inspected. Each value here with its two texts.
#[test]
fn a_value_is_shown_and_inspected_by_one_formatter() {
    let text = |s: &str| Value::String(s.into());
    let point = record_type(9200, "Point", &["y", "x"]);
    let println = crate::builtins::registry::registry()
        .named("println")
        .expect("println")
        .value();
    let cases: Vec<(Value, &str, &str)> = vec![
        (Value::Unit, "()", "()"),
        (Value::Int(-7), "-7", "-7"),
        (float(1.5), "1.5", "1.5"),
        (Value::Bool(true), "true", "true"),
        (text("a b"), "a b", "\"a b\""),
        // A tuple of one item is written as a program writes it.
        (Value::tuple(vec![Value::Int(1)]), "(1,)", "(1,)"),
        (Value::tuple(vec![text("x")]), "(x,)", "(\"x\",)"),
        (
            Value::tuple(vec![Value::tuple(vec![Value::Int(1)]), Value::Int(2)]),
            "((1,), 2)",
            "((1,), 2)",
        ),
        (
            Value::tuple(vec![Value::Int(1), text("x")]),
            "(1, x)",
            "(1, \"x\")",
        ),
        (Value::list(vec![]), "[]", "[]"),
        (
            Value::list(vec![text("a"), text("b")]),
            "[a, b]",
            "[\"a\", \"b\"]",
        ),
        (Value::List(ints(1, 3)), "[1, 2, 3]", "[1, 2, 3]"),
        (
            Value::List(ints(0, MAX_COLLECTION_LEN as i64)),
            "[0, 1, 2, ..., 10000000]",
            "[0, 1, 2, ..., 10000000]",
        ),
        // A key that is a string is in quotes however the map is
        // written; a key that has parts is written like any value.
        (
            Value::map([
                (text("k"), text("v")),
                (text("l"), Value::list(vec![Value::Int(1)])),
            ]),
            "#{\"k\": v, \"l\": [1]}",
            "#{\"k\": \"v\", \"l\": [1]}",
        ),
        (
            Value::map([(Value::tuple(vec![Value::Int(1), text("a")]), Value::Int(2))]),
            "#{(1, a): 2}",
            "#{(1, \"a\"): 2}",
        ),
        (Value::map([]), "#{}", "#{}"),
        (
            Value::set([text("b"), text("a")]),
            "#[a, b]",
            "#[\"a\", \"b\"]",
        ),
        // A record in the order its type declares its fields; an
        // anonymous one without a name; one without fields.
        (
            Value::record(point.clone(), vec![Value::Int(2), text("one")]),
            "Point {y: 2, x: one}",
            "Point {y: 2, x: \"one\"}",
        ),
        (
            Value::anon_record([("b", Value::Int(2)), ("a", text("x"))]),
            "{a: x, b: 2}",
            "{a: \"x\", b: 2}",
        ),
        (
            Value::record(record_type(9201, "Empty", &[]), vec![]),
            "Empty {}",
            "Empty {}",
        ),
        // A builtin record of time is shown as its text and inspected
        // as the record it is, its fields as its type declares them.
        (
            make_date(2024, 3, 9),
            "2024-03-09",
            "Date {year: 2024, month: 3, day: 9}",
        ),
        (
            make_time(7, 30, 5, 0),
            "07:30:05",
            "Time {hour: 7, minute: 30, second: 5, ns: 0}",
        ),
        (
            Value::list(vec![make_date(2024, 3, 9)]),
            "[2024-03-09]",
            "[Date {year: 2024, month: 3, day: 9}]",
        ),
        (Value::variant(bv::NONE, vec![]), "None", "None"),
        (
            Value::variant(bv::SOME, vec![text("x")]),
            "Some(x)",
            "Some(\"x\")",
        ),
        (
            Value::variant(bv::OK, vec![Value::tuple(vec![Value::Int(1), Value::Unit])]),
            "Ok((1, ()))",
            "Ok((1, ()))",
        ),
        // A builtin error is shown as its message.
        (
            Value::variant(bv::IO_NOT_FOUND, vec![text("f.txt")]),
            "file not found: f.txt",
            "NotFound(\"f.txt\")",
        ),
        (println, "<builtin:println>", "<fn>"),
        (
            Value::Bytes(Arc::new(vec![1, 255])),
            "bytes(01 ff, length: 2)",
            "bytes(01 ff, length: 2)",
        ),
    ];
    for (value, shown, inspected) in &cases {
        assert_eq!(&value.to_string(), shown);
        assert_eq!(&value.format_silt(), inspected);
        // Rust's `{:?}` is the value inspected.
        assert_eq!(&format!("{value:?}"), inspected);
    }
}

/// A value nested 100,000 levels deep is written like any other: the
/// formatter keeps what it has begun in a stack of its own. And it is
/// dropped like any other: the fields of an object far inside are put
/// aside and dropped by the outermost. (Each recursed once a level: a
/// thread of a test has a stack for a few thousand levels of it.)
#[test]
fn a_deep_value_is_written_and_dropped_without_the_native_stack() {
    const DEPTH: usize = 100_000;
    let node = record_type(9202, "Node", &["next", "tag"]);
    let shapes: [(&dyn Fn(Value) -> Value, &str, &str); 3] = [
        // A variant in a variant.
        (&|rest| Value::variant(bv::SOME, vec![rest]), "Some(", ")"),
        // A list in a variant, and a tuple of one in it.
        (
            &|rest| Value::variant(bv::OK, vec![Value::list(vec![Value::tuple(vec![rest])])]),
            "Ok([(",
            ",)])",
        ),
        // A record whose first field is the next level.
        (
            &|rest| Value::record(node.clone(), vec![rest, Value::Int(1)]),
            "Node {next: ",
            ", tag: 1}",
        ),
    ];
    for (wrap, open, close) in shapes {
        let value = (0..DEPTH).fold(Value::Int(7), |rest, _| wrap(rest));
        for text in [value.to_string(), value.format_silt()] {
            assert_eq!(text.len(), DEPTH * (open.len() + close.len()) + 1);
            assert!(text.starts_with(&open.repeat(3)));
            assert!(text.ends_with(&close.repeat(3)));
            assert!(text.contains(&format!("{open}7{close}")));
        }
        assert!(value.writable().is_ok());
        // The value is dropped here, a clone of its top first.
        let again = value.clone();
        drop(value);
        drop(again);
    }
    // A chain ten times as long, of a list of a program's own.
    let chain = (0..10 * DEPTH as i64).fold(Value::variant(bv::NONE, vec![]), |rest, n| {
        Value::variant(bv::SOME, vec![Value::tuple(vec![Value::Int(n), rest])])
    });
    drop(chain);
}

/// A value nested 200,000 levels deep through any holder of values is
/// dropped like any other: what a holder far inside holds is put aside
/// and dropped by the outermost (src/value/deep.rs). A program nests a
/// value through a list, a tuple, a map or a set alone with a function
/// that calls itself at another type, and through a closure, a channel
/// or the handle of a task with a loop. (Each of these was dropped
/// once a level on the native stack: a thread of a test has a stack
/// for a few thousand levels of it.)
#[test]
fn a_value_nested_through_any_holder_is_dropped_without_the_native_stack() {
    use crate::bytecode::{VmClosure, call_global_script};
    use crate::runtime::handle::TaskHandle;
    use crate::runtime::sync::{Channel, TaskId, TrySend, Wake};

    const DEPTH: i64 = 200_000;
    struct Nobody;
    impl Wake for Nobody {
        fn wake(&self, _: TaskId) {}
    }
    let function = Arc::new(call_global_script(0, "f"));
    let shapes: [&dyn Fn(i64, Value) -> Value; 10] = [
        // A list, and a part of a list.
        &|_, rest| Value::list(vec![rest]),
        &|n, rest| {
            let list = List::from(vec![Value::Int(n), rest, Value::Int(n)]);
            Value::List(list.slice(1, 3))
        },
        &|n, rest| Value::tuple(vec![Value::Int(n), rest]),
        // A map, of which the next level is a value, or a key; a set.
        &|n, rest| Value::map([(Value::Int(n), rest)]),
        &|n, rest| Value::map([(rest, Value::Int(n))]),
        &|_, rest| Value::set([rest]),
        // A closure that has captured the next level.
        &|_, rest| Value::VmClosure(Arc::new(VmClosure::new(function.clone(), vec![rest]))),
        // A channel in which the next level waits.
        &|n, rest| {
            let channel = Channel::new(n as usize, 1);
            assert!(matches!(channel.try_send(rest, &Nobody), TrySend::Sent));
            Value::Channel(channel)
        },
        // The handle of a task that ended with the next level.
        &|n, rest| {
            let handle = TaskHandle::new(n as usize);
            assert!(handle.complete(Ok(rest), &Nobody));
            Value::Handle(Arc::new(handle))
        },
        // One kind of holder in another.
        &|n, rest| match n % 4 {
            0 => Value::list(vec![rest]),
            1 => Value::tuple(vec![rest, Value::Int(n)]),
            2 => Value::set([rest]),
            _ => Value::variant(bv::SOME, vec![rest]),
        },
    ];
    for wrap in shapes {
        let value = (0..DEPTH).fold(Value::Int(7), |rest, n| wrap(n, rest));
        // The value is dropped here, a clone of its top first.
        let again = value.clone();
        drop(value);
        drop(again);
        // Nothing is left counted or put aside: a second one is
        // dropped as the first was.
        let value = (0..DEPTH).fold(Value::Int(7), |rest, n| wrap(n, rest));
        drop(value);
    }
}

#[test]
fn display_set() {
    let set = Value::set([Value::Int(1), Value::Int(2)]);
    assert_eq!(format!("{}", set), "#[1, 2]");
}

#[test]
fn display_builtin_fn() {
    assert_eq!(
        format!(
            "{}",
            crate::builtins::registry::registry()
                .named("println")
                .expect("println")
                .value()
        ),
        "<builtin:println>"
    );
}

// ── Lists ──────────────────────────────────────────────────────

/// The list that holds the Ints `items`.
fn made(items: impl IntoIterator<Item = i64>) -> List {
    items.into_iter().map(Value::Int).collect()
}

/// The list of the Ints from `lo` to `hi`, which holds none.
fn ints(lo: i64, hi: i64) -> List {
    List::ints(lo, hi).expect("a list")
}

fn elements(list: &List) -> Vec<Value> {
    list.to_vec().expect("few elements")
}

/// Each way of storing the Ints from 0 to 9.
fn both() -> [List; 2] {
    [made(0..10), ints(0, 9)]
}

#[test]
fn a_part_of_a_list_is_the_list_of_its_elements() {
    for xs in both() {
        let part = xs.slice(3, 7);
        assert_eq!(part.len(), 4);
        assert_eq!(elements(&part), elements(&made(3..7)));
        assert_eq!(part.get(0), Some(Value::Int(3)));
        assert_eq!(part.first(), Some(Value::Int(3)));
        assert_eq!(part.last(), Some(Value::Int(6)));
        assert_eq!(part.get(4), None);
        // A part of a part counts from the part's beginning.
        assert_eq!(elements(&part.slice(1, 3)), elements(&made(4..6)));
        assert_eq!(xs.len(), 10);
    }
}

/// Two lists as one: the elements of the first and then those of the
/// second, however each is stored, and a part of a list as well as a
/// whole one. A list that holds no element is made one that holds
/// them, if they are few enough.
#[test]
fn two_lists_as_one_list() {
    for xs in both() {
        for ys in [made(10..13), ints(10, 12)] {
            let joined = xs.concat(&ys).expect("few enough elements");
            assert_eq!(joined.len(), 13);
            assert_eq!(elements(&joined), elements(&made(0..13)));
            assert_eq!(xs.len(), 10);
            // Parts of lists.
            let parts = xs
                .slice(8, 10)
                .concat(&ys.slice(1, 2))
                .expect("three elements");
            assert_eq!(
                elements(&parts),
                [Value::Int(8), Value::Int(9), Value::Int(11)]
            );
        }
        let none = List::new();
        assert_eq!(
            elements(&xs.concat(&none).expect("the list")),
            elements(&xs)
        );
        assert_eq!(
            elements(&none.concat(&xs).expect("the list")),
            elements(&xs)
        );
        assert!(none.concat(&none).expect("no element").is_empty());
    }
    let cap = MAX_COLLECTION_LEN as i64;
    let long = ints(1, cap + 1);
    let short = made(0..1);
    assert!(long.concat(&short).is_err());
    assert!(short.concat(&long).is_err());
    assert!(long.concat(&List::new()).is_err());
}

/// A list with an element more at either end, with one element in
/// place of another, or without one: a new list, however the list is
/// stored and whether it is a part of another, and the list stays as
/// it was. An index past the end is no element; a list that holds no
/// element is made one that holds them if they are few enough.
#[test]
fn a_list_updated_is_a_new_list() {
    let n = Value::Int;
    for xs in both().into_iter().chain(both().map(|xs| xs.slice(2, 10))) {
        let mut want = elements(&xs);
        let first = want[0].clone();
        let pushed = xs.push_front(n(-1)).expect("few elements");
        want.insert(0, n(-1));
        assert_eq!(elements(&pushed), want);
        want.remove(0);
        let pushed = xs.push_back(n(-1)).expect("few elements");
        want.push(n(-1));
        assert_eq!(elements(&pushed), want);
        want.pop();
        let len = want.len();
        for at in [0, 3, len - 1] {
            let set = xs
                .set(at, n(-1))
                .expect("few elements")
                .expect("in the list");
            let mut changed = want.clone();
            changed[at] = n(-1);
            assert_eq!(elements(&set), changed);
            let removed = xs.remove_at(at).expect("few elements").expect("in it");
            let mut changed = want.clone();
            changed.remove(at);
            assert_eq!(elements(&removed), changed);
        }
        assert!(xs.set(len, n(-1)).expect("no error").is_none());
        assert!(xs.remove_at(len).expect("no error").is_none());
        assert_eq!(elements(&xs), want);
        assert_eq!(xs.first(), Some(first));
    }
    let long = ints(1, MAX_COLLECTION_LEN as i64 + 1);
    assert!(long.push_front(n(0)).is_err());
    assert!(long.push_back(n(0)).is_err());
    assert!(long.set(0, n(0)).is_err());
    assert!(long.remove_at(0).is_err());
    assert!(
        long.set(long.len(), n(0))
            .expect("no element there")
            .is_none()
    );
}

/// A part of a list shares the list's storage, and copies no element,
/// if it is a quarter of the storage or more, and has storage of its
/// own if it is less: a list keeps at most four times its own elements
/// from being freed. A short list keeps its elements in a buffer, a
/// long one made in one go side by side, a long one a change made in a
/// tree; a part of a tree that gets storage of its own gets a tree made
/// of the parts it shares with it, and no element is copied for it,
/// unless it is short enough for a buffer. Other parts are copied.
#[test]
fn a_short_part_of_a_list_does_not_keep_the_list() {
    use super::list::FLAT_MOST;
    for (n, kept) in [(FLAT_MOST, "buffer"), (1000, "side"), (1000, "tree")] {
        let big = match kept {
            "tree" => made(0..n as i64 - 1)
                .push_back(Value::Int(n as i64 - 1))
                .expect("few elements"),
            _ => made(0..n as i64),
        };
        assert_eq!(big.kept_as(), kept);
        let (len, at) = big.buffer().expect("a list that holds its elements");
        assert_eq!(len, n);
        let quarter = n.div_ceil(4);

        // The rest after the first element, and any part of a quarter
        // or more, is a part of the same storage.
        for (from, to) in [(1, n), (0, quarter), (n - quarter, n)] {
            let mut part = List::new();
            assert_eq!(copies_of(|| part = big.slice(from, to)), 0, "{from}..{to}");
            assert_eq!(part.buffer(), Some((n, at)), "{from}..{to}");
        }
        // A shorter part holds its own elements and no others.
        for (from, to) in [(0, 1), (n - 1, n), (3, 3 + quarter - 1), (0, quarter - 1)] {
            let mut part = List::new();
            let copied = copies_of(|| part = big.slice(from, to));
            let (held, apart) = part.buffer().expect("a list that holds its elements");
            assert_eq!(held, to - from, "{from}..{to}");
            assert_ne!(apart, at);
            assert_eq!(elements(&part), elements(&made(from as i64..to as i64)));
            match part.kept_as() {
                "tree" => assert_eq!(copied, 0, "{from}..{to}"),
                _ => assert_eq!(copied, (to - from) as u64, "{from}..{to}"),
            }
        }
        // A part of a part is measured against the storage, not the
        // part.
        let half = big.slice(n / 2, n);
        let rest = half.slice(1, quarter + 1);
        assert_eq!(rest.buffer(), Some((n, at)));
        let rest = half.slice(1, quarter);
        assert_eq!(rest.buffer().map(|(held, _)| held), Some(quarter - 1));
    }
    // A list that holds no element has no storage to keep.
    assert_eq!(ints(0, 999).slice(3, 4).buffer(), None);
}

/// A walk down a list by its rest (`[h, ..t]`, `list.tail`) copies
/// few of its elements, whatever its length: each rest is the same
/// storage until it is less than a quarter of it. A list in a buffer
/// copies a third of itself in all (a quarter of it once it is that
/// short, a quarter of that, and so on), and so does a long list made
/// in one go; a list in a tree takes a tree of its own a quarter of the
/// length each time, copying nothing, and copies only once it is short
/// enough for a buffer.
#[test]
fn a_walk_down_a_list_copies_few_of_its_elements() {
    use super::list::FLAT_MOST;
    for (n, kept) in [
        (FLAT_MOST, "buffer"),
        (1_000, "side"),
        (64_000, "side"),
        (1_000, "tree"),
        (64_000, "tree"),
    ] {
        let mut rest = match kept {
            "tree" => made(0..n as i64 - 1)
                .push_back(Value::Int(n as i64 - 1))
                .expect("few elements"),
            _ => made(0..n as i64),
        };
        assert_eq!(rest.kept_as(), kept);
        let mut buffer = rest.buffer();
        let mut storages = 0;
        let copied = copies_of(|| {
            while !rest.is_empty() {
                rest = rest.slice(1, rest.len());
                if rest.buffer() != buffer && !rest.is_empty() {
                    storages += 1;
                }
                buffer = rest.buffer();
            }
        });
        let most = match kept {
            // At most the elements of a buffer a tree crosses back to,
            // and a third of that.
            "tree" => FLAT_MOST / 2 + FLAT_MOST / 6,
            _ => n / 3,
        };
        assert!(
            copied as usize <= most,
            "{copied} elements copied in a walk of {n}"
        );
        assert!(
            copied as usize >= FLAT_MOST / 8,
            "{copied} elements copied in a walk of {n}"
        );
        assert!(storages <= 10, "{storages} storages in a walk of {n}");
    }
}

#[test]
fn a_bound_of_a_part_past_the_end_is_the_end() {
    for xs in both() {
        assert_eq!(elements(&xs.slice(8, 99)), elements(&made(8..10)));
        assert!(xs.slice(10, 10).is_empty());
        assert!(xs.slice(99, 100).is_empty());
        assert!(xs.slice(3, 1).is_empty());
    }
    assert!(List::new().slice(0, 1).is_empty());
    assert!(List::new().first().is_none());
    assert!(List::new().last().is_none());
}

#[test]
fn a_list_is_read_from_both_ends() {
    for xs in both() {
        let part = xs.slice(1, 5);
        let forward: Vec<Value> = part.iter().collect();
        let backward: Vec<Value> = part.iter().rev().collect();
        assert_eq!(forward, elements(&made(1..5)));
        assert_eq!(backward, elements(&made((1..5).rev())));
        assert_eq!(part.iter().len(), 4);
        let owned: Vec<Value> = part.into_iter().collect();
        assert_eq!(owned, forward);
    }
}

#[test]
fn a_list_is_equal_ordered_and_hashed_by_its_elements_however_it_is_stored() {
    let stored = [
        Value::List(made(0..6).slice(2, 5)),
        Value::List(made(2..5)),
        Value::List(ints(2, 4)),
        Value::List(ints(0, 9).slice(2, 5)),
    ];
    for a in &stored {
        assert_eq!(a.to_string(), "[2, 3, 4]");
        assert_eq!(format!("{a:?}"), "[2, 3, 4]");
        assert_eq!(a.format_silt(), "[2, 3, 4]");
        for b in &stored {
            assert_eq!(a, b);
            assert_eq!(a.cmp(b), Ordering::Equal);
            assert_eq!(hash_of(a), hash_of(b));
        }
        // A list that goes on is the greater, one that begins higher
        // too, however each is stored.
        for longer in [made(2..6), ints(2, 5)] {
            assert_eq!(a.cmp(&Value::List(longer.clone())), Ordering::Less);
            assert_eq!(Value::List(longer).cmp(a), Ordering::Greater);
        }
        for higher in [made(3..4), ints(3, 3)] {
            assert_eq!(a.cmp(&Value::List(higher.clone())), Ordering::Less);
            assert_eq!(Value::List(higher).cmp(a), Ordering::Greater);
        }
        assert_ne!(
            *a,
            Value::list(vec![Value::Int(2), Value::Int(3), Value::Int(5)])
        );
    }
    // One element, and none.
    assert_eq!(Value::List(ints(7, 7)), Value::List(made(7..8)));
    assert_eq!(
        hash_of(&Value::List(ints(7, 7))),
        hash_of(&Value::List(made(7..8)))
    );
    assert_eq!(Value::List(ints(5, 1)), Value::List(List::new()));
    assert_eq!(
        hash_of(&Value::List(ints(5, 1))),
        hash_of(&Value::List(List::new()))
    );
    // Lists that differ hash apart (these two did not, when a range
    // over the cap was hashed by its ends and a list by its elements).
    assert_ne!(
        hash_of(&Value::List(made(0..5))),
        hash_of(&Value::List(made([0, 1, 2, 3, 5])))
    );
}

#[test]
fn a_list_of_the_ints_between_two_ends_holds_none_of_them() {
    let all = ints(1, i64::MAX);
    assert_eq!(all.len(), i64::MAX as usize);
    assert_eq!(all.get(0), Some(Value::Int(1)));
    assert_eq!(all.last(), Some(Value::Int(i64::MAX)));
    assert_eq!(all.get(i64::MAX as usize), None);
    assert!(all.contains(&Value::Int(77)));
    assert!(!all.contains(&Value::Int(0)));
    assert!(!all.contains(&Value::Unit));
    assert_eq!(all.position(&Value::Int(77)), Some(76));
    assert_eq!(all.position(&Value::Int(0)), None);
    let rest = all.slice(1, all.len());
    assert_eq!(rest.first(), Some(Value::Int(2)));
    assert_eq!(rest.len(), i64::MAX as usize - 1);
    let end = all.slice(all.len() - 2, all.len());
    assert_eq!(elements(&end), elements(&made([i64::MAX - 1, i64::MAX])));
    // Its hash and its order are read off its ends.
    assert_eq!(
        hash_of(&Value::List(all.clone())),
        hash_of(&Value::List(ints(1, i64::MAX)))
    );
    assert_ne!(
        hash_of(&Value::List(all.clone())),
        hash_of(&Value::List(rest.clone()))
    );
    assert!(Value::List(all.clone()) < Value::List(rest));
    assert!(Value::List(all.slice(0, 5)) < Value::List(all.clone()));
    // The least Int, and the greatest, as ends.
    let low = ints(i64::MIN, i64::MIN + 8);
    assert_eq!(low.len(), 9);
    assert_eq!(low.slice(9, 9).len(), 0);
    assert_eq!(low.last(), Some(Value::Int(i64::MIN + 8)));
}

#[test]
fn a_list_has_at_most_as_many_elements_as_its_length_counts() {
    // More elements than an Int counts, by one: a list.
    let naturals = ints(0, i64::MAX);
    assert_eq!(naturals.len(), 1 << 63);
    assert_eq!(naturals.last(), Some(Value::Int(i64::MAX)));
    // All the Ints but the greatest: the index of an element can be
    // more than an Int holds.
    let most = ints(i64::MIN, i64::MAX - 1);
    assert_eq!(most.len(), usize::MAX);
    assert_eq!(most.get(usize::MAX - 1), Some(Value::Int(i64::MAX - 1)));
    assert_eq!(most.get(usize::MAX), None);
    assert_eq!(most.position(&Value::Int(5)), Some((1 << 63) + 5));
    let upper = most.slice((1 << 63) + 5, usize::MAX);
    assert_eq!(upper.first(), Some(Value::Int(5)));
    assert_eq!(upper.last(), Some(Value::Int(i64::MAX - 1)));
    // All the Ints there are: one too many.
    let too_long = List::ints(i64::MIN, i64::MAX).expect_err("one too many");
    assert_eq!(
        too_long.to_string(),
        "range -9223372036854775808..9223372036854775807 has 18446744073709551616 elements: \
         a list has at most 18446744073709551615"
    );
    assert!(List::ints(i64::MAX, i64::MIN).expect("empty").is_empty());
}

#[test]
fn the_elements_of_a_long_list_that_holds_none_are_not_made() {
    let cap = MAX_COLLECTION_LEN as i64;
    assert_eq!(ints(1, cap).writable(), Ok(()));
    let long = ints(0, cap);
    assert_eq!(
        long.to_vec().expect_err("too long").to_string(),
        "range 0..10000000 has 10000001 elements; materializing more than 10000000 is not allowed"
    );
    assert_eq!(long.writable(), long.to_vec().map(|_| ()));
    // A value that holds such a list is not written out for a program;
    // a host that formats it gets the ends.
    let holder = Value::tuple(vec![Value::Int(1), Value::List(long.clone())]);
    assert_eq!(holder.writable(), long.writable());
    assert_eq!(holder.to_string(), "(1, [0, 1, 2, ..., 10000000])");
    assert_eq!(holder.format_silt(), "(1, [0, 1, 2, ..., 10000000])");
    assert_eq!(format!("{holder:?}"), "(1, [0, 1, 2, ..., 10000000])");
    // A list that holds its elements is written out whatever its
    // length.
    assert_eq!(Value::List(made(0..4)).writable(), Ok(()));
}

fn total(total: IntTotal) -> Result<i64, &'static str> {
    match total {
        IntTotal::Total(total) => Ok(total),
        IntTotal::Overflow => Err("overflow"),
        IntTotal::NotInts => Err("not ints"),
    }
}

#[test]
fn the_sum_of_a_list_of_ints() {
    let sum = |list: &List| total(list.sum_ints());
    for xs in both() {
        assert_eq!(sum(&xs), Ok(45));
    }
    assert_eq!(sum(&List::new()), Ok(0));
    assert_eq!(sum(&ints(-5, 5)), Ok(0));
    assert_eq!(sum(&ints(-7, -3)), Ok(-25));
    assert_eq!(sum(&ints(1, 4_294_967_295)), Ok(9_223_372_034_707_292_160));
    assert_eq!(sum(&ints(1, 4_294_967_296)), Err("overflow"));
    assert_eq!(sum(&ints(1, i64::MAX)), Err("overflow"));
    assert_eq!(sum(&ints(i64::MIN, -2)), Err("overflow"));
    assert_eq!(sum(&ints(i64::MIN + 1, i64::MAX)), Ok(0));
    assert_eq!(sum(&ints(i64::MIN + 1, i64::MAX - 1)), Ok(-i64::MAX));
    assert_eq!(sum(&ints(i64::MIN, i64::MAX - 1)), Err("overflow"));
    assert_eq!(sum(&ints(i64::MIN, i64::MAX - 2)), Err("overflow"));
    assert_eq!(sum(&made([i64::MAX, 1])), Err("overflow"));
    assert_eq!(
        sum(&[Value::Int(1), Value::Unit].into_iter().collect()),
        Err("not ints")
    );
}

#[test]
fn the_product_of_a_list_of_ints() {
    let product = |list: &List| total(list.product_ints());
    for xs in both() {
        assert_eq!(product(&xs), Ok(0));
    }
    assert_eq!(product(&List::new()), Ok(1));
    // However a list is stored, its product is the same.
    for (lo, hi) in [(1, 20), (1, 21), (-20, -1), (-21, -1), (-3, -1), (7, 7)] {
        assert_eq!(
            product(&ints(lo, hi)),
            product(&made(lo..=hi)),
            "{lo}..{hi}"
        );
    }
    assert_eq!(product(&ints(1, 20)), Ok(2_432_902_008_176_640_000));
    assert_eq!(product(&ints(1, 21)), Err("overflow"));
    assert_eq!(product(&ints(-3, -1)), Ok(-6));
    assert_eq!(product(&ints(i64::MIN, i64::MIN)), Ok(i64::MIN));
    assert_eq!(product(&ints(i64::MIN, i64::MIN + 1)), Err("overflow"));
    // A 0 among the elements is the product, and no element of a list
    // that holds none is visited for it: the product of the others may
    // be no `Int`, and there may be nine quintillion of them.
    for (lo, hi) in [(-100, 100), (0, 100), (-100, 0), (0, 0)] {
        assert_eq!(product(&ints(lo, hi)), Ok(0), "{lo}..{hi}");
        assert_eq!(product(&made(lo..=hi)), Ok(0), "{lo}..{hi}");
    }
    assert_eq!(product(&ints(0, i64::MAX)), Ok(0));
    assert_eq!(product(&ints(i64::MIN, 0)), Ok(0));
    assert_eq!(product(&ints(i64::MIN + 1, i64::MAX)), Ok(0));
    assert_eq!(product(&ints(1, i64::MAX)), Err("overflow"));
    assert_eq!(product(&ints(i64::MIN, -1)), Err("overflow"));
    assert_eq!(product(&made([i64::MAX, 2])), Err("overflow"));
    assert_eq!(product(&made([i64::MAX, 2, 0])), Ok(0));
    assert_eq!(
        product(&[Value::Int(1), Value::Unit].into_iter().collect()),
        Err("not ints")
    );
}

#[test]
fn a_list_in_order_and_without_its_repeats() {
    for xs in both() {
        assert_eq!(Value::List(xs.sorted()), Value::List(xs.clone()));
        assert_eq!(Value::List(xs.unique()), Value::List(xs));
    }
    let mixed = made([3, 1, 3, 2, 1]);
    assert_eq!(elements(&mixed.sorted()), elements(&made([1, 1, 2, 3, 3])));
    assert_eq!(elements(&mixed.unique()), elements(&made([3, 1, 2])));
    assert!(List::new().sorted().is_empty());
    assert!(List::new().unique().is_empty());
    // A list that holds no element is in order and has none twice:
    // none of its elements is made, however many there are.
    let long = ints(1, i64::MAX);
    assert!(long.to_vec().is_err());
    assert_eq!(long.sorted().len(), long.len());
    assert_eq!(long.sorted().last(), Some(Value::Int(i64::MAX)));
    assert_eq!(long.unique().len(), long.len());
    assert_eq!(long.unique().get(41), Some(Value::Int(42)));
}

/// A list's key, its text and the walks of the run time read the
/// elements it holds as runs of them side by side in memory
/// (`List::runs`): one run while a list is held whole in one buffer,
/// several where a list is stored in pieces. Where the runs end is
/// nothing these show. Lists whose runs end in different places (one
/// run against three), nested in each other, near the top of a value
/// and below where the walk takes a stack of its own, are equal,
/// ordered, hashed and written as their twins of one run each are.
#[test]
fn where_a_list_s_runs_end_is_nothing_a_value_shows() {
    use super::list::cut::runs_at_most;
    // `value` with each list in it a part of a buffer with `off`
    // elements before it: its runs end at other places than those of
    // a list in a buffer of its own.
    fn moved(value: &Value, off: usize) -> Value {
        match value {
            Value::List(list) if list.range_ends().is_none() => {
                let items: Vec<Value> = list.iter().map(|item| moved(&item, off)).collect();
                let len = items.len();
                let buf = std::iter::repeat_n(Value::Unit, off).chain(items);
                let part = List::from(buf.collect::<Vec<_>>()).slice(off, off + len);
                if len > 0 {
                    assert_eq!(part.buffer().map(|(n, _)| n), Some(off + len));
                }
                Value::List(part)
            }
            other => other.clone(),
        }
    }
    let runs = |list: &Value| -> Vec<usize> {
        let Value::List(list) = list else {
            panic!("no list: {list:?}")
        };
        list.runs().map(<[Value]>::len).collect()
    };
    let four = Value::list((0..4).map(Value::Int).collect());
    assert_eq!(runs(&four), [4]);
    runs_at_most(2, || {
        assert_eq!(runs(&four), [2, 2]);
        assert_eq!(runs(&moved(&four, 1)), [1, 2, 1]);
        assert_eq!(runs(&moved(&four, 2)), [2, 2]);
        assert_eq!(runs(&moved(&four, 3)), [1, 2, 1]);
    });
    runs_at_most(3, || assert_eq!(runs(&moved(&four, 2)), [1, 3]));

    // Every list of up to three of these, and three lists that hold no
    // element.
    let alphabet = [
        Value::Int(0),
        Value::Int(1),
        Value::list(vec![Value::Int(0)]),
        Value::VariantConstructor(bv::SOME.into()),
    ];
    let mut lists = vec![Vec::new()];
    let mut longer = vec![Vec::new()];
    for _ in 0..3 {
        longer = longer
            .iter()
            .flat_map(|list: &Vec<Value>| {
                alphabet.iter().map(move |item| {
                    let mut list = list.clone();
                    list.push(item.clone());
                    list
                })
            })
            .collect();
        lists.extend(longer.iter().cloned());
    }
    let mut values: Vec<Value> = lists.into_iter().map(Value::list).collect();
    for (lo, hi) in [(0, 1), (0, 2), (1, 3)] {
        values.push(Value::List(List::ints(lo, hi).expect("a short range")));
    }
    assert_eq!(values.len(), 85 + 3);

    // What a value shows, and of two which comes first.
    let shown = |a: &Value, b: &Value| {
        (
            a == b,
            a.cmp(b),
            hash_of(a),
            hash_of(b),
            format!("{a} {a:?} {b} {b:?}"),
            a.contains_fn(),
            a.writable().is_ok(),
        )
    };
    // Where the runs end, and how far into its buffer each list of the
    // two begins.
    let cuts = [(1, 0, 1), (2, 0, 1), (2, 3, 0), (3, 1, 2), (5, 0, 3)];
    let near = super::key::NEAR;
    // A list of three around `inner`, `levels` deep.
    let deep = |inner: &Value, levels: usize| {
        (0..levels).fold(inner.clone(), |inner, _| {
            Value::list(vec![Value::Int(7), inner, Value::Int(8)])
        })
    };
    let mut compared = 0;
    for (at, a) in values.iter().enumerate() {
        for (bt, b) in values.iter().enumerate() {
            let mut pairs = vec![(a.clone(), b.clone())];
            // (Below `NEAR` for a sample of the pairs.)
            if (at * values.len() + bt).is_multiple_of(23) {
                pairs.push((deep(a, near + 2), deep(b, near + 2)));
            }
            for (a, b) in pairs {
                let expected = shown(&a, &b);
                for (most, a_off, b_off) in cuts {
                    let (a, b) = (moved(&a, a_off), moved(&b, b_off));
                    let got = runs_at_most(most, || shown(&a, &b));
                    assert_eq!(got, expected, "{a:?} and {b:?}, runs of {most}");
                    compared += 1;
                }
            }
        }
    }
    assert!(compared > 88 * 88 * cuts.len());
}

// ── Lists kept in trees ────────────────────────────────────────

/// How many elements `f` copies, or moves, into storage it makes for
/// lists ([`super::list::copies`]).
fn copies_of(f: impl FnOnce()) -> u64 {
    let before = super::list::copies::count();
    f();
    super::list::copies::count() - before
}

/// A list built, or changed, one element at a time copies a number of
/// elements that grows like `n * log2(n)` at most, not like `n * n`:
/// an element more at the front (`[x, ..xs]`, which is a list of one
/// joined to the list), at the back (`list.append`), one in place of
/// another (`list.set`), a short list joined at either end
/// (`list.concat`), one taken out (`list.remove_at`). Each old list
/// is kept until the new one is made, as a fold keeps it. (Copying
/// the whole list at each step, as a list kept in one buffer must,
/// copies 50,000,000 elements for 10,000 of them.) So do `n` changes
/// of one long list made in one go, each from the list as it was.
#[test]
fn a_list_changed_an_element_at_a_time_copies_n_log_n_elements() {
    // The most elements copied per element and per doubling of the
    // length.
    const C: f64 = 2.0;
    type Step = fn(&List, i64) -> List;
    let steps: [(&str, Step); 8] = [
        ("[x, ..xs]", |xs, i| {
            made([i]).concat(xs).expect("few enough elements")
        }),
        ("list.append", |xs, i| {
            xs.push_back(Value::Int(i)).expect("few enough elements")
        }),
        ("list.prepend", |xs, i| {
            xs.push_front(Value::Int(i)).expect("few enough elements")
        }),
        ("list.concat(xs, short)", |xs, i| {
            xs.concat(&made([i, i])).expect("few enough elements")
        }),
        ("list.concat(short, xs)", |xs, i| {
            made([i, i]).concat(xs).expect("few enough elements")
        }),
        ("[..xs, x]", |xs, i| {
            xs.concat(&made([i])).expect("few enough elements")
        }),
        ("list.concat(xs, i..i + 1)", |xs, i| {
            xs.concat(&ints(i, i + 1)).expect("few enough elements")
        }),
        ("list.concat(i..i + 1, xs)", |xs, i| {
            ints(i, i + 1).concat(xs).expect("few enough elements")
        }),
    ];
    for n in [10_000usize, 20_000] {
        let bound = (C * n as f64 * (n as f64).log2()) as u64;
        for (name, step) in steps {
            let mut list = List::new();
            let copied = copies_of(|| {
                for i in 0..n as i64 {
                    let next = step(&list, i);
                    list = next;
                }
            });
            assert!(list.len() >= n, "{name}");
            assert!(
                copied <= bound,
                "{name} of {n}: {copied} copied, at most {bound}"
            );
        }
        let all = made(0..n as i64);
        let mut list = all.clone();
        let copied = copies_of(|| {
            for i in 0..n {
                let next = list.set(i, Value::Int(-1)).expect("few elements");
                list = next.expect("an element there");
            }
        });
        assert!(
            copied <= bound,
            "list.set of {n}: {copied} copied, at most {bound}"
        );
        assert!(list.iter().all(|item| item == Value::Int(-1)));
        let mut list = all.clone();
        let copied = copies_of(|| {
            while !list.is_empty() {
                let next = list.remove_at(list.len() / 2).expect("few elements");
                list = next.expect("an element there");
            }
        });
        assert!(
            copied <= bound,
            "list.remove_at of {n}: {copied} copied, at most {bound}"
        );
        // `n` changes of one long list made in one go, and of a part of
        // it, each made from the list as it was (`list.set(board, i, x)`
        // for every `i`): its tree is made once, by the first.
        type Change = fn(&List, usize) -> List;
        let changes: [(&str, Change); 5] = [
            ("list.set", |xs, i| {
                let set = xs.set(i % xs.len(), Value::Int(-1));
                set.expect("few elements").expect("an element there")
            }),
            ("list.append", |xs, i| {
                xs.push_back(Value::Int(i as i64)).expect("few elements")
            }),
            ("list.prepend", |xs, i| {
                xs.push_front(Value::Int(i as i64)).expect("few elements")
            }),
            ("list.remove_at", |xs, i| {
                let removed = xs.remove_at(i % xs.len());
                removed.expect("few elements").expect("an element there")
            }),
            ("[x, ..xs]", |xs, i| {
                made([i as i64]).concat(xs).expect("few elements")
            }),
        ];
        for (name, change) in changes {
            for base in [made(0..n as i64), made(0..n as i64).slice(1, n)] {
                let copied = copies_of(|| {
                    for i in 0..n {
                        let changed = change(&base, i);
                        assert_eq!(changed.kept_as(), "tree", "{name}");
                    }
                });
                assert!(
                    copied <= bound,
                    "{name} of one list of {n}, {n} times: {copied} copied, at most {bound}"
                );
            }
        }
    }
}

/// A list the model test makes: the Ints from `from` (each one more
/// than the one before, or not, as `ascending` says), `len` of them,
/// held, or (`ints`) a list that holds none where they ascend.
#[derive(Debug, Clone)]
struct Made {
    from: i64,
    len: usize,
    ascending: bool,
    ints: bool,
}

impl Made {
    fn items(&self) -> Vec<Value> {
        (0..self.len as i64)
            .map(|i| match self.ascending {
                true => Value::Int(self.from + i),
                false => Value::Int((self.from + i * 7).rem_euclid(13)),
            })
            .collect()
    }

    fn list(&self) -> List {
        match self.ints && self.ascending && self.len > 0 {
            true => ints(self.from, self.from + self.len as i64 - 1),
            false => List::from(self.items()),
        }
    }
}

/// A change the model test makes to its list.
#[derive(Debug, Clone)]
enum Change {
    PushFront(i64),
    PushBack(i64),
    /// At an index, counted modulo one more than the length.
    Set(usize, i64),
    RemoveAt(usize),
    /// A part, its bounds counted modulo one more than the length.
    Slice(usize, usize),
    Rest,
    Append(Made),
    Prepend(Made),
    Start(Made),
}

fn made_strategy() -> impl proptest::strategy::Strategy<Value = Made> {
    use proptest::prelude::*;
    let len = prop_oneof![0usize..40, 0usize..200, 0usize..1500];
    (-5i64..60, len, any::<bool>(), any::<bool>()).prop_map(|(from, len, ascending, ints)| Made {
        from,
        len,
        ascending,
        ints,
    })
}

fn change_strategy() -> impl proptest::strategy::Strategy<Value = Change> {
    use proptest::prelude::*;
    let n = -3i64..40;
    prop_oneof![
        3 => n.clone().prop_map(Change::PushFront),
        3 => n.clone().prop_map(Change::PushBack),
        2 => (any::<usize>(), n).prop_map(|(at, x)| Change::Set(at, x)),
        2 => any::<usize>().prop_map(Change::RemoveAt),
        2 => (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Change::Slice(a, b)),
        3 => Just(Change::Rest),
        1 => made_strategy().prop_map(Change::Append),
        1 => made_strategy().prop_map(Change::Prepend),
        1 => made_strategy().prop_map(Change::Start),
    ]
}

/// The list `list`, whose elements are `model`, is the list of its
/// elements whatever it is stored in: its elements read in every way
/// there is, and it is equal to, ordered with, hashed and written as
/// a list of the same elements held in one buffer is; it is ordered
/// with `before` as `model` is with `before_model`; and it keeps at
/// most four times its own elements from being freed.
fn the_list_is_its_model(list: &List, model: &[Value], before: &List, before_model: &[Value]) {
    use super::list::FLAT_MOST;
    let len = model.len();
    assert_eq!(list.len(), len);
    assert_eq!(list.is_empty(), model.is_empty());
    assert_eq!(list.iter().collect::<Vec<_>>(), model);
    assert!(list.iter().rev().eq(model.iter().rev().cloned()));
    assert_eq!(list.iter().len(), len);
    assert_eq!(list.clone().into_iter().collect::<Vec<_>>(), model);
    assert_eq!(list.to_vec().expect("few elements"), model);
    match list.range_ends() {
        Some((lo, hi)) => assert!(model.iter().cloned().eq((lo..=hi).map(Value::Int))),
        None => assert!(list.runs().flatten().eq(model.iter())),
    }
    for at in [0, len / 3, len / 2, len.saturating_sub(1), len] {
        assert_eq!(list.get(at), model.get(at).cloned(), "element {at}");
    }
    assert_eq!(list.first(), model.first().cloned());
    assert_eq!(list.last(), model.last().cloned());
    for x in [-1, 0, 7, 12, 39] {
        let x = Value::Int(x);
        assert_eq!(list.position(&x), model.iter().position(|item| *item == x));
    }
    let sum = model.iter().map(|item| match item {
        Value::Int(n) => *n,
        _ => unreachable!(),
    });
    match list.sum_ints() {
        IntTotal::Total(total) => assert_eq!(total, sum.sum::<i64>()),
        _ => panic!("the sum of a few small Ints"),
    }
    let mut sorted = model.to_vec();
    sorted.sort();
    assert_eq!(elements(&list.sorted()), sorted);
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<Value> = model.iter().filter(|x| seen.insert(*x)).cloned().collect();
    assert_eq!(elements(&list.unique()), unique);

    let (value, twin) = (
        Value::List(list.clone()),
        Value::List(List::held_flat(model.to_vec())),
    );
    assert!(value == twin);
    assert_eq!(value.cmp(&twin), Ordering::Equal);
    assert_eq!(hash_of(&value), hash_of(&twin));
    assert_eq!(format!("{value} {value:?}"), format!("{twin} {twin:?}"));
    let before_value = Value::List(before.clone());
    assert_eq!(value.cmp(&before_value), model.cmp(before_model));
    assert_eq!(value == before_value, model == before_model);
    if model == before_model {
        assert_eq!(hash_of(&value), hash_of(&before_value));
    }

    if let Some((held, _)) = list.buffer() {
        assert!(held < 4 * len.max(1) + 4, "{held} kept for {len}");
        match list.kept_as() {
            "tree" => assert!(held > FLAT_MOST / 2, "a tree of {held}"),
            "side" => assert!(held > FLAT_MOST, "a long list of {held}"),
            _ => assert!(held <= FLAT_MOST, "a buffer of {held}"),
        }
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(400))]

    /// Every change a list can be made, in any order, from lists of
    /// every kind and length (in a buffer, in a tree, at either side of
    /// the length between and in the gap where a list stays what it
    /// was, a list that holds none), to parts of lists and to parts of
    /// parts: the list made is the list of the elements a `Vec` that is
    /// changed alike holds ([`the_list_is_its_model`]).
    #[test]
    fn a_list_is_the_list_of_its_elements_however_it_is_kept(
        start in made_strategy(),
        changes in proptest::collection::vec(change_strategy(), 1..40),
    ) {
        let mut list = start.list();
        let mut model = start.items();
        the_list_is_its_model(&list, &model, &list, &model);
        for change in changes {
            let len = model.len();
            let (next, next_model) = match change {
                Change::PushFront(x) => {
                    let mut model = model.clone();
                    model.insert(0, Value::Int(x));
                    (list.push_front(Value::Int(x)).expect("few elements"), model)
                }
                Change::PushBack(x) => {
                    let mut model = model.clone();
                    model.push(Value::Int(x));
                    (list.push_back(Value::Int(x)).expect("few elements"), model)
                }
                Change::Set(at, x) => {
                    let at = at % (len + 1);
                    let set = list.set(at, Value::Int(x)).expect("few elements");
                    let mut model = model.clone();
                    match model.get_mut(at) {
                        Some(item) => *item = Value::Int(x),
                        None => assert!(set.is_none()),
                    }
                    (set.unwrap_or_else(|| list.clone()), model)
                }
                Change::RemoveAt(at) => {
                    let at = at % (len + 1);
                    let removed = list.remove_at(at).expect("few elements");
                    let mut model = model.clone();
                    match at < len {
                        true => {
                            model.remove(at);
                        }
                        false => assert!(removed.is_none()),
                    }
                    (removed.unwrap_or_else(|| list.clone()), model)
                }
                Change::Slice(a, b) => {
                    let (a, b) = (a % (len + 1), b % (len + 1));
                    let part = model[a.min(b)..b.max(a)].to_vec();
                    (list.slice(a.min(b), b.max(a)), part)
                }
                Change::Rest => {
                    let part = model.get(1..).unwrap_or_default().to_vec();
                    (list.slice(1, len), part)
                }
                Change::Append(other) => {
                    let mut model = model.clone();
                    model.extend(other.items());
                    (list.concat(&other.list()).expect("few elements"), model)
                }
                Change::Prepend(other) => {
                    let mut items = other.items();
                    items.extend(model.iter().cloned());
                    (other.list().concat(&list).expect("few elements"), items)
                }
                Change::Start(other) => (other.list(), other.items()),
            };
            the_list_is_its_model(&next, &next_model, &list, &model);
            // (The list changed is as it was.)
            the_list_is_its_model(&list, &model, &next, &next_model);
            (list, model) = (next, next_model);
        }
    }
}

/// A value nested 10,000 levels deep through long lists is
/// dropped like any other, as one nested through any other holder is
/// (`a_value_nested_through_any_holder_is_dropped_without_the_native_stack`):
/// a tree far inside is put aside and dropped by the outermost
/// (src/value/deep.rs). A level is a long list made in one go, one
/// whose tree a change has made and kept, a part of a list in a tree,
/// or one made by an element more. (Each was dropped once
/// a level on the native stack, and a thread of a test overflowed its
/// stack at a few thousand levels; a level of a tree costs kilobytes,
/// so the depth that a program's thread overflows at would take
/// gigabytes to reach.)
#[test]
fn a_value_nested_through_lists_kept_in_trees_is_dropped_without_the_native_stack() {
    use super::list::FLAT_MOST;
    const DEPTH: i64 = 10_000;
    let long = |n: i64, rest: Value| -> List {
        let mut items: Vec<Value> = (0..FLAT_MOST as i64 + 6).map(Value::Int).collect();
        let at = n as usize % items.len();
        items[at] = rest;
        List::from(items)
    };
    let shapes: [&dyn Fn(i64, Value) -> Value; 4] = [
        &|n, rest| Value::List(long(n, rest)),
        &|n, rest| {
            let list = long(n, rest);
            let changed = list.set(0, Value::Unit).expect("few elements");
            assert_eq!(changed.map(|changed| changed.kept_as()), Some("tree"));
            Value::List(list)
        },
        &|n, rest| {
            let list = long(n, Value::Int(n))
                .push_front(rest)
                .expect("few elements");
            Value::List(list.slice(0, FLAT_MOST + 3))
        },
        &|n, rest| {
            Value::List(
                long(n, Value::Int(n))
                    .push_back(rest)
                    .expect("few elements"),
            )
        },
    ];
    for wrap in shapes {
        let value = (0..DEPTH).fold(Value::Int(7), |rest, n| wrap(n, rest));
        let Value::List(top) = &value else {
            panic!("a list")
        };
        assert_ne!(top.kept_as(), "buffer");
        let again = value.clone();
        drop(value);
        drop(again);
    }
}

/// A change of a long list that holds no element (`0..n`) writes its
/// elements out side by side, with the change, as a list made in one go
/// keeps them: building a tree of them element by element took twelve
/// times as long, and `n` changes of one range cost what writing it
/// out `n` times costs. The next change of the list made starts from a
/// tree made once, so a chain of changes from a range copies about as
/// many elements as one from any other list. A short range changed is
/// a buffer.
#[test]
fn a_changed_range_is_written_out_side_by_side() {
    let n = 10_000usize;
    let bound = (2.0 * n as f64 * (n as f64).log2()) as u64;
    let range = ints(0, n as i64 - 1);
    let x = Value::Int(-1);
    let changes = [
        (
            "list.set",
            range.set(5, x.clone()).expect("few").expect("there"),
        ),
        ("list.append", range.push_back(x.clone()).expect("few")),
        ("list.prepend", range.push_front(x.clone()).expect("few")),
        (
            "list.remove_at",
            range.remove_at(5).expect("few").expect("there"),
        ),
        ("[x, ..xs]", made([-1]).concat(&range).expect("few")),
        ("[..xs, x]", range.concat(&made([-1])).expect("few")),
    ];
    for (name, changed) in changes {
        assert_eq!(changed.kept_as(), "side", "{name}");
        let mut list = changed;
        let copied = copies_of(|| {
            for i in 0..n {
                let at = i % list.len();
                let next = list.set(at, Value::Int(i as i64)).expect("few elements");
                list = next.expect("an element there");
            }
        });
        assert_eq!(list.kept_as(), "tree", "{name}");
        assert!(
            copied <= bound,
            "{name}, then {n} changes: {copied} copied, at most {bound}"
        );
    }
    let short = ints(0, 9).push_back(x).expect("few elements");
    assert_eq!(short.kept_as(), "buffer");
    assert_eq!(elements(&short).len(), 11);
}
