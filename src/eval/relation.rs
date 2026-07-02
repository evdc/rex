//! The `BinaryRelation` storage abstraction and a BTree-backed implementation.
//!
//! A binary relation is a Z-set over `(left, right)` pairs with integer weights
//! (§2): weight 1 = "present once", negative = a retraction, >1 = multiplicity.
//! The algebra (see `algebra.rs`) is written against this trait, so the backing
//! store can be swapped without touching it.

use super::value::Value;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

pub trait BinaryRelation {
    /// Add `weight` to the pair `(left, right)`. Entries whose weight reaches 0
    /// are pruned, so "present" always means nonzero weight.
    fn add(&mut self, left: Value, right: Value, weight: i64);

    /// The weight of a specific pair (0 if absent).
    fn weight(&self, left: &Value, right: &Value) -> i64;

    /// Number of stored (nonzero) pairs.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// All `(left, right, weight)` triples with nonzero weight.
    fn iter(&self) -> Box<dyn Iterator<Item = (Value, Value, i64)> + '_>;

    /// The `(right, weight)` entries for a fixed `left` — the join index.
    fn row(&self, left: &Value) -> Box<dyn Iterator<Item = (Value, i64)> + '_>;

    /// Whether `left` is a key of this relation (its domain).
    fn has_left(&self, left: &Value) -> bool;

    /// The distinct left keys.
    fn domain(&self) -> Box<dyn Iterator<Item = Value> + '_>;
}

/// A BTree-backed relation, indexed left -> right -> weight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BTreeRelation {
    index: BTreeMap<Value, BTreeMap<Value, i64>>,
    len: usize,
}

impl BTreeRelation {
    pub fn new() -> BTreeRelation {
        BTreeRelation::default()
    }

    /// Build from `(left, right, weight)` triples.
    pub fn from_triples(triples: impl IntoIterator<Item = (Value, Value, i64)>) -> BTreeRelation {
        let mut r = BTreeRelation::new();
        for (l, rt, w) in triples {
            r.add(l, rt, w);
        }
        r
    }

    /// Collect entries in sorted order (handy for tests and printing).
    pub fn to_sorted_vec(&self) -> Vec<(Value, Value, i64)> {
        self.iter().collect()
    }

    /// Borrowing counterpart of [`BinaryRelation::iter`]: no boxing, no
    /// `Value` clones. The hot-path API for the incremental kernels.
    pub fn triples(&self) -> impl Iterator<Item = (&Value, &Value, i64)> {
        self.index.iter().flat_map(|(l, row)| row.iter().map(move |(r, w)| (l, r, *w)))
    }

    /// Borrowing counterpart of [`BinaryRelation::row`].
    pub fn row_ref<'s>(&'s self, left: &Value) -> impl Iterator<Item = (&'s Value, i64)> + use<'s> {
        self.index.get(left).into_iter().flatten().map(|(r, w)| (r, *w))
    }

    /// The row map at `left`, if any — a re-iterable probe for kernels that
    /// scan one row several times.
    pub fn row_map(&self, left: &Value) -> Option<&BTreeMap<Value, i64>> {
        self.index.get(left)
    }

    /// Borrowing counterpart of [`BinaryRelation::domain`].
    pub fn keys(&self) -> impl Iterator<Item = &Value> {
        self.index.keys()
    }

    /// Borrowing iterator over `(left key, row map)` pairs — one pass over the
    /// relation grouped by key, without re-probing per key.
    pub fn rows(&self) -> impl Iterator<Item = (&Value, &BTreeMap<Value, i64>)> {
        self.index.iter()
    }
}

impl BinaryRelation for BTreeRelation {
    fn add(&mut self, left: Value, right: Value, weight: i64) {
        if weight == 0 {
            return;
        }
        match self.index.entry(left) {
            Entry::Vacant(e) => {
                e.insert(BTreeMap::from([(right, weight)]));
                self.len += 1;
            }
            Entry::Occupied(mut e) => {
                let row = e.get_mut();
                match row.get_mut(&right) {
                    Some(w) => {
                        *w += weight;
                        if *w == 0 {
                            row.remove(&right);
                            self.len -= 1;
                            // Keep the domain accurate: drop a row that just
                            // became empty.
                            if row.is_empty() {
                                e.remove();
                            }
                        }
                    }
                    None => {
                        row.insert(right, weight);
                        self.len += 1;
                    }
                }
            }
        }
    }

    fn weight(&self, left: &Value, right: &Value) -> i64 {
        self.index
            .get(left)
            .and_then(|row| row.get(right))
            .copied()
            .unwrap_or(0)
    }

    fn len(&self) -> usize {
        self.len
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (Value, Value, i64)> + '_> {
        Box::new(self.index.iter().flat_map(|(l, row)| {
            row.iter().map(move |(r, w)| (l.clone(), r.clone(), *w))
        }))
    }

    fn row(&self, left: &Value) -> Box<dyn Iterator<Item = (Value, i64)> + '_> {
        match self.index.get(left) {
            Some(row) => Box::new(row.iter().map(|(r, w)| (r.clone(), *w))),
            None => Box::new(std::iter::empty()),
        }
    }

    fn has_left(&self, left: &Value) -> bool {
        self.index.get(left).is_some_and(|row| !row.is_empty())
    }

    fn domain(&self) -> Box<dyn Iterator<Item = Value> + '_> {
        Box::new(self.index.keys().cloned())
    }
}
