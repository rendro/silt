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
        Value::variant(Tag::named(ty, name).expect("a variant"), vec![])
    };
    assert!(value(&a, "Red") < value(&a, "Blue"));
    assert!(value(&b, "Blue") < value(&b, "Red"));
    assert_ne!(value(&a, "Red"), value(&b, "Red"));
    assert_eq!(hash_of(&value(&a, "Red")), hash_of(&value(&a, "Red")));
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
    assert!(Arc::ptr_eq(a, b));
    assert_eq!(a[..], [text.clone(), Value::Int(1)]);
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
    let cap = MAX_RANGE_MATERIALIZE as i64;
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
        assert_eq!(xs.sorted(), xs);
        assert_eq!(xs.unique(), xs);
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
