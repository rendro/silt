//! Tables that are cheap to copy however large they are, for what a
//! REPL session hands on from entry to entry: a map ([`Layers`]) and a
//! list ([`Rows`]) whose rows are kept in layers, each layer shared by
//! every copy made while it was the newest. A copy copies no row; a
//! row added to a copy goes to a layer of that copy's own.
//!
//! Each layer is more than twice as large as the one after it: two
//! that are not are made one. So a table of `n` rows has at most
//! `log2(n) + 1` layers, a row is copied `O(log n)` times however many
//! copies are made between two rows, and a table nothing was copied
//! from while it was built (a program's, compiled in one go) has one
//! layer.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash};
use std::sync::Arc;

#[cfg(any(test, feature = "test-hooks"))]
thread_local! {
    static COPIED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test-only: how many rows this thread has copied so far to keep its
/// tables and a VM's global slots: the rows of two layers made one, and
/// the values of slots moved to an array with more room. What a REPL
/// entry adds to it does not grow with the session's length.
#[cfg(any(test, feature = "test-hooks"))]
pub fn rows_copied() -> u64 {
    COPIED.with(std::cell::Cell::get)
}

/// Note that `rows` rows were copied ([`rows_copied`]).
pub(crate) fn count_copied(rows: usize) {
    #[cfg(any(test, feature = "test-hooks"))]
    COPIED.with(|copied| copied.set(copied.get() + rows as u64));
    #[cfg(not(any(test, feature = "test-hooks")))]
    let _ = rows;
}

/// Add a row to the newest layer of `layers` with `add`, in a layer of
/// its own (`alone`) if a copy shares that layer, and make one layer of
/// the last two (`join`) while the older is not more than twice the
/// newer.
fn add_row<L: Clone>(
    layers: &mut Vec<Arc<L>>,
    add: impl FnOnce(&mut L),
    alone: impl FnOnce() -> L,
    len: impl Fn(&L) -> usize,
    join: impl Fn(&mut L, L),
) {
    match layers.last_mut().and_then(Arc::get_mut) {
        Some(newest) => add(newest),
        None => {
            let mut layer = alone();
            add(&mut layer);
            layers.push(Arc::new(layer));
        }
    }
    while let [.., older, newer] = &layers[..]
        && len(older) <= 2 * len(newer)
    {
        let newer = layers.pop().expect("the newer of two layers");
        let older = layers.pop().expect("the older of two layers");
        count_copied(len(&older) + len(&newer));
        let mut joined = Arc::unwrap_or_clone(older);
        join(&mut joined, Arc::unwrap_or_clone(newer));
        layers.push(Arc::new(joined));
    }
}

/// A map that is cheap to copy. A key is given its value once.
#[derive(Debug)]
pub struct Layers<K, V, S = RandomState> {
    /// Oldest first, which is largest first.
    layers: Vec<Arc<HashMap<K, V, S>>>,
}

impl<K, V, S> Default for Layers<K, V, S> {
    fn default() -> Self {
        Layers { layers: Vec::new() }
    }
}

impl<K, V, S> Clone for Layers<K, V, S> {
    fn clone(&self) -> Self {
        Layers {
            layers: self.layers.clone(),
        }
    }
}

impl<K, V, S> Layers<K, V, S>
where
    K: Eq + Hash + Clone,
    V: Clone,
    S: BuildHasher + Default + Clone,
{
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.layers.iter().find_map(|layer| layer.get(key))
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Give `key` its value. (A key that has one keeps it: the layers
    /// are asked oldest first.)
    pub fn insert(&mut self, key: K, value: V) {
        debug_assert!(self.get(&key).is_none(), "a key is given its value once");
        add_row(
            &mut self.layers,
            |layer| {
                layer.insert(key, value);
            },
            HashMap::default,
            HashMap::len,
            |older, newer| older.extend(newer),
        );
    }
}

/// A list that is cheap to copy, and that grows at its end.
#[derive(Debug)]
pub struct Rows<T> {
    /// Oldest first, which is largest first: the rows of the list in
    /// order.
    layers: Vec<Arc<Vec<T>>>,
    len: usize,
}

impl<T> Default for Rows<T> {
    fn default() -> Self {
        Rows {
            layers: Vec::new(),
            len: 0,
        }
    }
}

impl<T> Clone for Rows<T> {
    fn clone(&self) -> Self {
        Rows {
            layers: self.layers.clone(),
            len: self.len,
        }
    }
}

impl<T: Clone> Rows<T> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, mut index: usize) -> Option<&T> {
        for layer in &self.layers {
            match layer.get(index) {
                Some(row) => return Some(row),
                None => index -= layer.len(),
            }
        }
        None
    }

    pub fn push(&mut self, row: T) {
        self.len += 1;
        add_row(
            &mut self.layers,
            |layer| layer.push(row),
            Vec::new,
            Vec::len,
            |older, newer| older.extend(newer),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table built with a copy taken after every `every`th row: what
    /// a REPL session does to its globals, entry by entry. The copies
    /// are kept, so no layer a copy has is ever changed in place.
    fn built(rows: usize, every: usize) -> (Layers<usize, usize>, Rows<usize>, usize) {
        let (mut map, mut list) = (Layers::default(), Rows::default());
        let mut copies = Vec::new();
        let mut most_layers = 0;
        for row in 0..rows {
            map.insert(row, row * 3);
            list.push(row * 3);
            most_layers = most_layers.max(map.layers.len()).max(list.layers.len());
            if row % every == 0 {
                copies.push((row + 1, map.clone(), list.clone()));
            }
        }
        // A copy has the rows it was made with, and none added since.
        for (len, map, list) in &copies {
            assert_eq!(list.len(), *len);
            assert_eq!(map.get(&(len - 1)), Some(&((len - 1) * 3)));
            assert_eq!(list.get(len - 1), Some(&((len - 1) * 3)));
            assert_eq!((map.get(len), list.get(*len)), (None, None));
        }
        (map, list, most_layers)
    }

    #[test]
    fn a_copied_table_keeps_every_row_in_few_layers() {
        for every in [1, 2, 7, 1000] {
            let (map, list, most_layers) = built(5000, every);
            assert_eq!(list.len(), 5000);
            for row in 0..5000 {
                assert_eq!(map.get(&row), Some(&(row * 3)), "every {every}");
                assert_eq!(list.get(row), Some(&(row * 3)), "every {every}");
            }
            assert_eq!((map.get(&5000), list.get(5000)), (None, None));
            // log2(5000) + 1
            assert!(most_layers <= 13, "{most_layers} layers, every {every}");
        }
    }

    #[test]
    fn a_table_nothing_was_copied_from_has_one_layer() {
        let (mut map, mut list) = (Layers::<u16, u16>::default(), Rows::default());
        assert!(map.is_empty());
        for row in 0..1000 {
            map.insert(row, row);
            list.push(row);
        }
        assert_eq!((map.layers.len(), list.layers.len()), (1, 1));
        assert!(!map.is_empty());
    }

    /// The rows copied while a table grows, with a copy taken after
    /// every row, are `O(n log n)` in all: a row is not copied again
    /// with every copy of the table.
    #[test]
    fn a_row_is_copied_a_logarithmic_number_of_times() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COPIES: AtomicUsize = AtomicUsize::new(0);
        struct Counted;
        impl Clone for Counted {
            fn clone(&self) -> Self {
                COPIES.fetch_add(1, Ordering::Relaxed);
                Counted
            }
        }
        let rows = 4096;
        let mut list = Rows::default();
        let mut copies = Vec::new();
        for _ in 0..rows {
            list.push(Counted);
            copies.push(list.clone());
        }
        // Each row is in a layer at least half as large again after
        // each time it is copied: at most log1.5(4096) = 21 times.
        let copied = COPIES.load(Ordering::Relaxed);
        assert!(copied <= rows * 21, "{copied} rows copied for {rows}");
    }
}
