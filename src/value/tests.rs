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
    let mut fields = BTreeMap::new();
    fields.insert("year".to_string(), Value::Int(year));
    fields.insert("month".to_string(), Value::Int(month));
    fields.insert("day".to_string(), Value::Int(day));
    Value::builtin_record(ty::DATE, fields)
}

fn make_time(hour: i64, minute: i64, second: i64, ns: i64) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("hour".to_string(), Value::Int(hour));
    fields.insert("minute".to_string(), Value::Int(minute));
    fields.insert("second".to_string(), Value::Int(second));
    fields.insert("ns".to_string(), Value::Int(ns));
    Value::builtin_record(ty::TIME, fields)
}

// ── Hash/Eq consistency ────────────────────────────────────────

#[test]
fn hash_eq_float_zero_and_neg_zero() {
    let pos = Value::Float(0.0);
    let neg = Value::Float(-0.0);
    assert_eq!(pos, neg, "0.0 and -0.0 should be equal");
    assert_eq!(hash_of(&pos), hash_of(&neg), "0.0 and -0.0 must hash equal");
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
    let a = Value::List(Arc::new(vec![Value::Int(1), Value::Int(2)]));
    let b = Value::List(Arc::new(vec![Value::Int(1), Value::Int(2)]));
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
    let a = Value::List(Arc::new(vec![]));
    let b = Value::List(Arc::new(vec![]));
    assert_eq!(a, b);
}

#[test]
fn nested_list_eq() {
    let inner1 = Value::List(Arc::new(vec![Value::Int(1)]));
    let inner2 = Value::List(Arc::new(vec![Value::Int(1)]));
    let a = Value::List(Arc::new(vec![inner1]));
    let b = Value::List(Arc::new(vec![inner2]));
    assert_eq!(a, b);
}

#[test]
fn different_variant_types_not_equal() {
    assert_ne!(Value::Int(1), Value::Float(1.0));
    assert_ne!(Value::Int(0), Value::Bool(false));
    assert_ne!(Value::String("1".into()), Value::Int(1));
}

#[test]
fn unit_eq() {
    assert_eq!(Value::Unit, Value::Unit);
}

#[test]
fn tuple_eq() {
    let a = Value::Tuple(vec![Value::Int(1), Value::String("x".into())]);
    let b = Value::Tuple(vec![Value::Int(1), Value::String("x".into())]);
    assert_eq!(a, b);
}

#[test]
fn tuple_neq_different_lengths() {
    let a = Value::Tuple(vec![Value::Int(1)]);
    let b = Value::Tuple(vec![Value::Int(1), Value::Int(2)]);
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
fn ord_float_normal() {
    assert!(Value::Float(1.0) < Value::Float(2.0));
    assert!(Value::Float(-1.0) < Value::Float(0.0));
}

#[test]
fn ord_float_nan_fallback() {
    let nan = Value::Float(f64::NAN);
    let _ = nan.cmp(&Value::Float(0.0));
    let _ = nan.cmp(&nan);
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
        Value::Variant(Tag::named(ty, name).expect("a variant"), vec![])
    };
    assert!(value(&a, "Red") < value(&a, "Blue"));
    assert!(value(&b, "Blue") < value(&b, "Red"));
    assert_ne!(value(&a, "Red"), value(&b, "Red"));
    assert_eq!(hash_of(&value(&a, "Red")), hash_of(&value(&a, "Red")));
}

/// A program's record type named like a builtin one prints as a
/// record: Display is keyed by the builtin type's id, not its name.
#[test]
fn a_program_type_named_time_is_not_the_builtin_time() {
    use crate::defs::{DefId, TypeId};
    let ty = TypeInfo::new_record(TypeId(DefId(9002)), "Time", Vec::new());
    let mut fields = BTreeMap::new();
    fields.insert("h".to_string(), Value::Int(1));
    let rec = Value::Record(ty, Arc::new(fields));
    assert_eq!(format!("{rec}"), "Time {h: 1}");
}

#[test]
fn ord_cross_type_by_discriminant() {
    assert!(Value::Unit < Value::Bool(true));
    assert!(Value::Bool(false) < Value::Int(0));
    assert!(Value::Int(0) < Value::Float(0.0));
}

// ── Display formatting ─────────────────────────────────────────

#[test]
fn display_int() {
    assert_eq!(format!("{}", Value::Int(42)), "42");
    assert_eq!(format!("{}", Value::Int(-1)), "-1");
}

#[test]
fn display_float() {
    assert_eq!(format!("{}", Value::Float(4.25)), "4.25");
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
    let list = Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
    assert_eq!(format!("{}", list), "[1, 2, 3]");
}

#[test]
fn display_empty_list() {
    let list = Value::List(Arc::new(vec![]));
    assert_eq!(format!("{}", list), "[]");
}

#[test]
fn display_range() {
    assert_eq!(format!("{}", Value::Range(1, 10)), "1..10");
}

#[test]
fn display_tuple() {
    let tuple = Value::Tuple(vec![Value::Int(1), Value::String("x".into())]);
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
    let mut fields = BTreeMap::new();
    fields.insert("x".to_string(), Value::Int(10));
    fields.insert("y".to_string(), Value::Int(20));
    let ty = TypeInfo::new_record(
        crate::defs::TypeId(crate::defs::DefId(9003)),
        "Point",
        Vec::new(),
    );
    let rec = Value::Record(ty, Arc::new(fields));
    assert_eq!(format!("{}", rec), "Point {x: 10, y: 20}");
}

#[test]
fn display_set() {
    let mut s = BTreeSet::new();
    s.insert(Value::Int(1));
    s.insert(Value::Int(2));
    let set = Value::Set(Arc::new(s));
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
