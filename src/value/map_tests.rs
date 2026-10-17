//! Maps and sets however they are kept: side by side or in a tree
//! (src/value/map.rs).

use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use super::map::FLAT_MOST;
use super::{Map, Set, Value};

fn hash_of(value: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// How many entries and members `f` copies, or moves, into storage it
/// makes for maps and sets ([`super::map::copies`]).
fn copies_of(f: impl FnOnce()) -> u64 {
    let before = super::map::copies::count();
    f();
    super::map::copies::count() - before
}

fn int(n: i64) -> Value {
    Value::Int(n)
}

/// The key of the model tests: an Int, or a text for some, so that
/// keys of two kinds are ordered too.
fn key(n: i64) -> Value {
    match n % 5 {
        0 => Value::String(format!("k{n}").into()),
        _ => int(n),
    }
}

/// The most elements `n` changes copy, at `C` per element and per
/// doubling of the size.
fn bound(n: usize) -> u64 {
    const C: f64 = 2.0;
    (C * n as f64 * (n as f64).log2()) as u64
}

// ── Copies: the cost of a change grows with the log of the size ───

/// A map or a set built, or changed, one entry at a time copies a
/// number of entries that grows like `n * log2(n)` at most, not like
/// `n * n`: `map.set` of a new key and of a key it has
/// (`map.update` stores so), `map.delete`, `set.insert`, `set.remove`,
/// `map.merge` and `set.union` of a large one and a small one either
/// way round. Each old version is kept until the new one is made, as a
/// fold keeps it. (Copying the whole map at each step, as a map kept
/// in one std BTreeMap must, copies 50,000,000 entries for 10,000.)
#[test]
fn a_map_or_a_set_changed_an_entry_at_a_time_copies_n_log_n_entries() {
    let mut over = Vec::new();
    for n in [10_000usize, 20_000] {
        let bound = bound(n);
        let mut check = |name: &str, copied: u64| {
            if copied > bound {
                over.push(format!("{name} of {n}: {copied} copied, at most {bound}"));
            }
        };

        let mut map = Map::new();
        check(
            "map.set",
            copies_of(|| {
                for i in 0..n as i64 {
                    let next = map.insert(int(i), int(i));
                    map = next;
                }
            }),
        );
        assert_eq!(map.len(), n);
        let full = map.clone();
        check(
            "map.update",
            copies_of(|| {
                for i in 0..n as i64 {
                    let now = map.get(&int(i)).cloned().expect("a value");
                    let next = map.insert(int(i), now);
                    map = next;
                }
            }),
        );
        check(
            "map.delete",
            copies_of(|| {
                for i in 0..n as i64 {
                    let next = map.remove(&int(i));
                    map = next;
                }
            }),
        );
        assert!(map.is_empty());
        let small = Map::from_iter((0..5).map(|i| (int(i * (n as i64 / 5)), int(-1))));
        check(
            "map.merge(large, small)",
            copies_of(|| {
                for _ in 0..n {
                    assert_eq!(full.merge(&small).len(), n);
                }
            }),
        );
        check(
            "map.merge(small, large)",
            copies_of(|| {
                for _ in 0..n {
                    assert_eq!(small.merge(&full).len(), n);
                }
            }),
        );

        let mut set = Set::new();
        check(
            "set.insert",
            copies_of(|| {
                for i in 0..n as i64 {
                    let next = set.insert(int(i));
                    set = next;
                }
            }),
        );
        assert_eq!(set.len(), n);
        let full = set.clone();
        check(
            "set.remove",
            copies_of(|| {
                for i in 0..n as i64 {
                    let next = set.remove(&int(i));
                    set = next;
                }
            }),
        );
        assert!(set.is_empty());
        let small = Set::from_iter((0..5).map(|i| int(i * (n as i64 / 5) + 1)));
        let fresh = Set::from_iter((0..5).map(|i| int(-1 - i)));
        for (name, a, b) in [
            ("set.union(large, small)", &full, &small),
            ("set.union(small, large)", &small, &full),
            ("set.union(large, new)", &full, &fresh),
            ("set.union(new, large)", &fresh, &full),
        ] {
            check(
                name,
                copies_of(|| {
                    for _ in 0..n {
                        assert!(a.union(b).len() >= n);
                    }
                }),
            );
        }
        for (name, a, b) in [
            ("set.intersection(large, small)", &full, &small),
            ("set.intersection(small, large)", &small, &full),
            ("set.difference(large, small)", &full, &small),
            ("set.symmetric_difference(large, small)", &full, &small),
            ("set.symmetric_difference(small, large)", &small, &full),
        ] {
            check(
                name,
                copies_of(|| {
                    for _ in 0..n {
                        let _ = (a.intersection(b), a.difference(b));
                        let _ = a.symmetric_difference(b);
                    }
                }),
            );
        }
    }
    assert!(over.is_empty(), "{over:#?}");
}

// ── The model: every operation against std's BTreeMap and BTreeSet ─

/// A map, or a set, the model tests make in one go: of these keys, in
/// this order, some given twice.
#[derive(Debug, Clone)]
struct Made(Vec<(i64, i64)>);

impl Made {
    fn map(&self) -> Map {
        self.0.iter().map(|&(k, v)| (key(k), int(v))).collect()
    }

    fn model(&self) -> BTreeMap<Value, Value> {
        self.0.iter().map(|&(k, v)| (key(k), int(v))).collect()
    }

    fn set(&self) -> Set {
        self.0.iter().map(|&(k, _)| key(k)).collect()
    }

    fn set_model(&self) -> BTreeSet<Value> {
        self.0.iter().map(|&(k, _)| key(k)).collect()
    }

    /// The map of a literal of the entries.
    fn literal(&self) -> Map {
        let items: Vec<Value> = self.0.iter().flat_map(|&(k, v)| [key(k), int(v)]).collect();
        Map::of_literal(&items)
    }
}

fn made_strategy() -> impl proptest::strategy::Strategy<Value = Made> {
    use proptest::prelude::*;
    let most = 3 * FLAT_MOST as i64;
    let entries = |most_len: usize| proptest::collection::vec((0..most, -3i64..50), 0..most_len);
    prop_oneof![entries(6), entries(2 * FLAT_MOST), entries(4 * FLAT_MOST)].prop_map(Made)
}

/// A change the model tests make.
#[derive(Debug, Clone)]
enum Change {
    Insert(i64, i64),
    /// Insert many, one at a time.
    InsertRun(i64, usize),
    Remove(i64),
    /// Remove many, one at a time.
    RemoveRun(i64, usize),
    /// `map.update`: the value of a key changed by a function.
    Update(i64),
    /// `map.merge` or `set.union`, the made one first or second.
    Merge(Made, bool),
    Intersection(Made, bool),
    Difference(Made, bool),
    SymmetricDifference(Made, bool),
    /// Start again from a map made in one go.
    Start(Made),
    /// Start again from a map literal.
    Literal(Made),
    /// Keep the entries of a key that is even (`map.filter`).
    Filter,
}

fn change_strategy() -> impl proptest::strategy::Strategy<Value = Change> {
    use proptest::prelude::*;
    let k = 0..3 * FLAT_MOST as i64;
    let run = 0..2 * FLAT_MOST;
    prop_oneof![
        4 => (k.clone(), -3i64..50).prop_map(|(k, v)| Change::Insert(k, v)),
        2 => (k.clone(), run.clone()).prop_map(|(k, n)| Change::InsertRun(k, n)),
        3 => k.clone().prop_map(Change::Remove),
        2 => (k.clone(), run).prop_map(|(k, n)| Change::RemoveRun(k, n)),
        2 => k.prop_map(Change::Update),
        2 => (made_strategy(), any::<bool>()).prop_map(|(m, first)| Change::Merge(m, first)),
        1 => (made_strategy(), any::<bool>()).prop_map(|(m, f)| Change::Intersection(m, f)),
        1 => (made_strategy(), any::<bool>()).prop_map(|(m, f)| Change::Difference(m, f)),
        1 => (made_strategy(), any::<bool>())
            .prop_map(|(m, f)| Change::SymmetricDifference(m, f)),
        1 => made_strategy().prop_map(Change::Start),
        1 => made_strategy().prop_map(Change::Literal),
        1 => Just(Change::Filter),
    ]
}

/// A key the model tests change from `k`, `i` steps on.
fn run_key(k: i64, i: usize) -> i64 {
    (k + 7 * i as i64) % (3 * FLAT_MOST as i64)
}

/// The map `map`, whose entries are `model`'s, is the map of its
/// entries whatever it is kept in: it reads as `model`, and it is equal
/// to, ordered with, hashed and written as the map of the same entries
/// kept the other way is; and it is ordered with `before` as a twin
/// of `map` is with a twin of `before`.
fn the_map_is_its_model(map: &Map, model: &BTreeMap<Value, Value>, before: &Map) {
    assert_eq!(map.len(), model.len());
    assert_eq!(map.is_empty(), model.is_empty());
    assert!(map.iter().eq(model.iter()));
    assert_eq!(map.iter().len(), model.len());
    assert!(map.keys().eq(model.keys()));
    assert!(map.values().eq(model.values()));
    let owned = map.clone().into_iter();
    assert_eq!(owned.size_hint(), (model.len(), Some(model.len())));
    assert!(owned.eq(model.iter().map(|(k, v)| (k.clone(), v.clone()))));
    for k in -1..3 * FLAT_MOST as i64 + 1 {
        assert_eq!(map.get(&key(k)), model.get(&key(k)), "key {k}");
        assert_eq!(map.contains_key(&key(k)), model.contains_key(&key(k)));
    }
    match map.is_tree() {
        true => assert!(map.len() > FLAT_MOST / 2, "a tree of {}", map.len()),
        false => assert!(map.len() <= FLAT_MOST, "{} side by side", map.len()),
    }

    let entries: Vec<(Value, Value)> = model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let twin = Map::kept_as(entries, map.is_tree());
    assert_ne!(twin.is_tree(), map.is_tree());
    let (value, twin) = (Value::Map(map.clone()), Value::Map(twin));
    assert!(value == twin);
    assert_eq!(value.cmp(&twin), Ordering::Equal);
    assert_eq!(hash_of(&value), hash_of(&twin));
    assert_eq!(format!("{value} {value:?}"), format!("{twin} {twin:?}"));

    let before_entries = before.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let before_twin = Value::Map(Map::kept_as(before_entries, !before.is_tree()));
    let before = Value::Map(before.clone());
    assert_eq!(value.cmp(&before), twin.cmp(&before_twin));
    assert_eq!(value == before, twin == before_twin);
}

/// [`the_map_is_its_model`] for a set.
fn the_set_is_its_model(set: &Set, model: &BTreeSet<Value>, before: &Set) {
    assert_eq!(set.len(), model.len());
    assert_eq!(set.is_empty(), model.is_empty());
    assert!(set.iter().eq(model.iter()));
    assert_eq!(set.iter().len(), model.len());
    let owned = set.clone().into_iter();
    assert_eq!(owned.size_hint(), (model.len(), Some(model.len())));
    assert!(owned.eq(model.iter().cloned()));
    for k in -1..3 * FLAT_MOST as i64 + 1 {
        assert_eq!(set.contains(&key(k)), model.contains(&key(k)), "member {k}");
    }
    match set.is_tree() {
        true => assert!(set.len() > FLAT_MOST / 2, "a tree of {}", set.len()),
        false => assert!(set.len() <= FLAT_MOST, "{} side by side", set.len()),
    }

    let twin = Set::kept_as(model.iter().cloned().collect(), set.is_tree());
    assert_ne!(twin.is_tree(), set.is_tree());
    let (value, twin) = (Value::Set(set.clone()), Value::Set(twin));
    assert!(value == twin);
    assert_eq!(value.cmp(&twin), Ordering::Equal);
    assert_eq!(hash_of(&value), hash_of(&twin));
    assert_eq!(format!("{value} {value:?}"), format!("{twin} {twin:?}"));

    let before_twin = Value::Set(Set::kept_as(
        before.iter().cloned().collect(),
        !before.is_tree(),
    ));
    let before = Value::Set(before.clone());
    assert_eq!(value.cmp(&before), twin.cmp(&before_twin));
    assert_eq!(value == before, twin == before_twin);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(300))]

    /// Every change a map can be made, in any order, from maps of every
    /// size (side by side, in a tree, at either side of the size
    /// between and in the gap where a map stays as it was kept): the
    /// map made is the map of the entries a `BTreeMap` that is changed
    /// alike holds ([`the_map_is_its_model`]).
    #[test]
    fn a_map_is_the_map_of_its_entries_however_it_is_kept(
        start in made_strategy(),
        changes in proptest::collection::vec(change_strategy(), 1..30),
    ) {
        let mut map = start.map();
        let mut model = start.model();
        the_map_is_its_model(&map, &model, &map);
        for change in changes {
            let (next, next_model) = match change {
                Change::Insert(k, v) => {
                    let mut model = model.clone();
                    model.insert(key(k), int(v));
                    (map.insert(key(k), int(v)), model)
                }
                Change::InsertRun(k, n) => {
                    let (mut map, mut model) = (map.clone(), model.clone());
                    for i in 0..n {
                        model.insert(key(run_key(k, i)), int(i as i64));
                        map = map.insert(key(run_key(k, i)), int(i as i64));
                    }
                    (map, model)
                }
                Change::Remove(k) => {
                    let mut model = model.clone();
                    model.remove(&key(k));
                    (map.remove(&key(k)), model)
                }
                Change::RemoveRun(k, n) => {
                    let (mut map, mut model) = (map.clone(), model.clone());
                    for i in 0..n {
                        model.remove(&key(run_key(k, i)));
                        map = map.remove(&key(run_key(k, i)));
                    }
                    (map, model)
                }
                Change::Update(k) => {
                    let now = match map.get(&key(k)) {
                        Some(Value::Int(n)) => *n,
                        _ => 0,
                    };
                    let mut model = model.clone();
                    model.insert(key(k), int(now + 1));
                    (map.insert(key(k), int(now + 1)), model)
                }
                Change::Merge(other, first) => {
                    let (other_map, other_model) = (other.map(), other.model());
                    match first {
                        true => {
                            let mut merged = other_model;
                            merged.extend(model.clone());
                            (other_map.merge(&map), merged)
                        }
                        false => {
                            let mut merged = model.clone();
                            merged.extend(other_model);
                            (map.merge(&other_map), merged)
                        }
                    }
                }
                Change::Start(made) => (made.map(), made.model()),
                Change::Literal(made) => (made.literal(), made.model()),
                Change::Filter => {
                    let even = |k: &Value| matches!(k, Value::Int(n) if n % 2 == 0);
                    let kept = map.clone().into_iter().filter(|(k, _)| even(k));
                    let mut model = model.clone();
                    model.retain(|k, _| even(k));
                    (kept.collect(), model)
                }
                // (Set changes: the map stays.)
                _ => (map.clone(), model.clone()),
            };
            the_map_is_its_model(&next, &next_model, &map);
            (map, model) = (next, next_model);
        }
    }

    /// [`a_map_is_the_map_of_its_entries_however_it_is_kept`] for a set,
    /// with the operations on two sets.
    #[test]
    fn a_set_is_the_set_of_its_members_however_it_is_kept(
        start in made_strategy(),
        changes in proptest::collection::vec(change_strategy(), 1..30),
    ) {
        let mut set = start.set();
        let mut model = start.set_model();
        the_set_is_its_model(&set, &model, &set);
        for change in changes {
            let both = |other: &Made, first: bool| match first {
                true => (other.set(), set.clone(), other.set_model(), model.clone()),
                false => (set.clone(), other.set(), model.clone(), other.set_model()),
            };
            let (next, next_model) = match change {
                Change::Insert(k, _) => {
                    let mut model = model.clone();
                    model.insert(key(k));
                    (set.insert(key(k)), model)
                }
                Change::InsertRun(k, n) => {
                    let (mut set, mut model) = (set.clone(), model.clone());
                    for i in 0..n {
                        model.insert(key(run_key(k, i)));
                        set = set.insert(key(run_key(k, i)));
                    }
                    (set, model)
                }
                Change::Remove(k) | Change::Update(k) => {
                    let mut model = model.clone();
                    model.remove(&key(k));
                    (set.remove(&key(k)), model)
                }
                Change::RemoveRun(k, n) => {
                    let (mut set, mut model) = (set.clone(), model.clone());
                    for i in 0..n {
                        model.remove(&key(run_key(k, i)));
                        set = set.remove(&key(run_key(k, i)));
                    }
                    (set, model)
                }
                Change::Merge(other, first) => {
                    let (a, b, a_model, b_model) = both(&other, first);
                    (a.union(&b), a_model.union(&b_model).cloned().collect())
                }
                Change::Intersection(other, first) => {
                    let (a, b, a_model, b_model) = both(&other, first);
                    (a.intersection(&b), a_model.intersection(&b_model).cloned().collect())
                }
                Change::Difference(other, first) => {
                    let (a, b, a_model, b_model) = both(&other, first);
                    assert_eq!(a.is_subset(&b), a_model.is_subset(&b_model));
                    assert_eq!(b.is_subset(&a), b_model.is_subset(&a_model));
                    (a.difference(&b), a_model.difference(&b_model).cloned().collect())
                }
                Change::SymmetricDifference(other, first) => {
                    let (a, b, a_model, b_model) = both(&other, first);
                    let model = a_model.symmetric_difference(&b_model).cloned().collect();
                    (a.symmetric_difference(&b), model)
                }
                Change::Start(made) => (made.set(), made.set_model()),
                Change::Literal(made) => {
                    let members = made.0.iter().map(|&(k, _)| key(k));
                    (Set::from_iter(members.rev()), made.set_model())
                }
                Change::Filter => {
                    let even = |k: &Value| matches!(k, Value::Int(n) if n % 2 == 0);
                    let mut model = model.clone();
                    model.retain(even);
                    (set.clone().into_iter().filter(even).collect(), model)
                }
            };
            assert!(set.is_subset(&set));
            the_set_is_its_model(&next, &next_model, &set);
            (set, model) = (next, next_model);
        }
    }
}
