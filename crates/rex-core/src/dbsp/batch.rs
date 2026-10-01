//! Deltas as flat, consolidated batches (P-3).
//!
//! A delta is transient: built once by a kernel, read by a few parents, then
//! dropped (or handed to the host as a view delta). A [`Batch`] is a plain
//! `Vec` of `(left, right, weight)` triples, **sorted by `(left, right)` and
//! consolidated** — equal pairs merged, zero weights dropped — once, when the
//! kernel finishes. That is DBSP's sorted-run representation: reading is a
//! slice walk, grouping by key is a run of equal lefts, and a probe is a
//! binary search. Kernels mostly emit in input order already, so the
//! consolidation sort is usually a linear `is_sorted` check.
//!
//! Integrals, which are probed and updated in place, keep an indexed form
//! ([`super::integral::Integral`]).

use crate::eval::relation::{BTreeRelation, BinaryRelation};
use crate::eval::value::Value;

/// One `(left, right, weight)` row of a batch.
pub type Row = (Value, Value, i64);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Batch {
    rows: Vec<Row>,
}

/// Sort by `(left, right)`, merge equal pairs, drop zero weights.
fn consolidate(rows: &mut Vec<Row>) {
    let sorted = rows.windows(2).all(|w| (&w[0].0, &w[0].1) <= (&w[1].0, &w[1].1));
    if !sorted {
        rows.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    }
    // `dedup_by` hands (later, kept-earlier): fold the later weight into the
    // earlier row and drop it.
    rows.dedup_by(|later, kept| {
        if later.0 == kept.0 && later.1 == kept.1 {
            kept.2 += later.2;
            true
        } else {
            false
        }
    });
    rows.retain(|r| r.2 != 0);
}

impl Batch {
    pub fn new() -> Batch {
        Batch::default()
    }

    /// Consolidate arbitrary rows into a batch.
    pub fn from_rows(mut rows: Vec<Row>) -> Batch {
        consolidate(&mut rows);
        Batch { rows }
    }

    /// Build from triples already sorted by `(left, right)` with distinct
    /// pairs and nonzero weights — an integral's iteration order.
    pub fn from_sorted(rows: Vec<Row>) -> Batch {
        debug_assert!(
            rows.windows(2).all(|w| (&w[0].0, &w[0].1) < (&w[1].0, &w[1].1)) && rows.iter().all(|r| r.2 != 0),
            "Batch::from_sorted needs sorted, distinct, nonzero rows"
        );
        Batch { rows }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn as_slice(&self) -> &[Row] {
        &self.rows
    }

    pub fn into_rows(self) -> Vec<Row> {
        self.rows
    }

    /// Every row, in `(left, right)` order, borrowed.
    pub fn triples(&self) -> impl Iterator<Item = (&Value, &Value, i64)> {
        self.rows.iter().map(|(l, r, w)| (l, r, *w))
    }

    /// The contiguous run of rows with left key `left` (empty if none).
    pub fn row_slice(&self, left: &Value) -> &[Row] {
        let start = self.rows.partition_point(|r| r.0 < *left);
        let len = self.rows[start..].partition_point(|r| r.0 == *left);
        &self.rows[start..start + len]
    }

    /// The `(right, weight)` entries at `left`, in right order.
    pub fn row_ref<'s>(&'s self, left: &Value) -> impl Iterator<Item = (&'s Value, i64)> + use<'s> {
        self.row_slice(left).iter().map(|(_, r, w)| (r, *w))
    }

    pub fn weight(&self, left: &Value, right: &Value) -> i64 {
        let row = self.row_slice(left);
        match row.binary_search_by(|r| r.1.cmp(right)) {
            Ok(i) => row[i].2,
            Err(_) => 0,
        }
    }

    pub fn has_left(&self, left: &Value) -> bool {
        !self.row_slice(left).is_empty()
    }

    /// The runs of equal left keys: `(key, rows at key)`, in key order.
    pub fn runs(&self) -> Runs<'_> {
        Runs { rest: &self.rows }
    }

    /// The distinct left keys, in order.
    pub fn keys(&self) -> impl Iterator<Item = &Value> {
        self.runs().map(|(k, _)| k)
    }

    pub fn to_sorted_vec(&self) -> Vec<Row> {
        self.rows.clone()
    }
}

/// Iterator over a batch's runs of equal left keys.
pub struct Runs<'a> {
    rest: &'a [Row],
}

impl<'a> Iterator for Runs<'a> {
    type Item = (&'a Value, &'a [Row]);

    fn next(&mut self) -> Option<Self::Item> {
        let first = self.rest.first()?;
        let len = self.rest.iter().position(|r| r.0 != first.0).unwrap_or(self.rest.len());
        let (run, rest) = self.rest.split_at(len);
        self.rest = rest;
        Some((&first.0, run))
    }
}

/// A builder for a kernel's output: push rows in any order, then
/// [`finish`](Self::finish) consolidates once.
#[derive(Default)]
pub struct BatchBuilder {
    rows: Vec<Row>,
}

impl BatchBuilder {
    pub fn new() -> BatchBuilder {
        BatchBuilder::default()
    }

    pub fn with_capacity(n: usize) -> BatchBuilder {
        BatchBuilder { rows: Vec::with_capacity(n) }
    }

    pub fn push(&mut self, left: Value, right: Value, weight: i64) {
        if weight != 0 {
            self.rows.push((left, right, weight));
        }
    }

    pub fn finish(self) -> Batch {
        Batch::from_rows(self.rows)
    }
}

impl PartialEq<BTreeRelation> for Batch {
    fn eq(&self, other: &BTreeRelation) -> bool {
        self.len() == other.len() && self.triples().eq(other.triples())
    }
}

impl PartialEq<Batch> for BTreeRelation {
    fn eq(&self, other: &Batch) -> bool {
        other == self
    }
}

/// The batch as a general relation, for hosts and tests. `add` keeps the
/// batch consolidated with an O(n) insert, so it is not a kernel path.
impl BinaryRelation for Batch {
    fn add(&mut self, left: Value, right: Value, weight: i64) {
        if weight == 0 {
            return;
        }
        let at = self.rows.partition_point(|r| (&r.0, &r.1) < (&left, &right));
        match self.rows.get_mut(at) {
            Some(r) if r.0 == left && r.1 == right => {
                r.2 += weight;
                if r.2 == 0 {
                    self.rows.remove(at);
                }
            }
            _ => self.rows.insert(at, (left, right, weight)),
        }
    }

    fn weight(&self, left: &Value, right: &Value) -> i64 {
        Batch::weight(self, left, right)
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (Value, Value, i64)> + '_> {
        Box::new(self.rows.iter().cloned())
    }

    fn row(&self, left: &Value) -> Box<dyn Iterator<Item = (Value, i64)> + '_> {
        Box::new(self.row_slice(left).iter().map(|(_, r, w)| (r.clone(), *w)))
    }

    fn has_left(&self, left: &Value) -> bool {
        Batch::has_left(self, left)
    }

    fn domain(&self) -> Box<dyn Iterator<Item = Value> + '_> {
        Box::new(self.keys().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: i64) -> Value {
        Value::Int(n)
    }

    #[test]
    fn consolidates_unsorted_rows() {
        let b = Batch::from_rows(vec![
            (int(2), int(1), 1),
            (int(1), int(5), 2),
            (int(1), int(5), -2),
            (int(1), int(3), 1),
            (int(2), int(1), 1),
        ]);
        assert_eq!(b.to_sorted_vec(), vec![(int(1), int(3), 1), (int(2), int(1), 2)]);
        assert_eq!(b.weight(&int(2), &int(1)), 2);
        assert_eq!(b.weight(&int(1), &int(5)), 0);
        assert!(!b.has_left(&int(3)));
        let runs: Vec<_> = b.runs().map(|(k, rows)| (k.clone(), rows.len())).collect();
        assert_eq!(runs, vec![(int(1), 1), (int(2), 1)]);
    }

    #[test]
    fn add_keeps_it_consolidated() {
        let mut b = Batch::new();
        b.add(int(2), int(2), 1);
        b.add(int(1), int(1), 1);
        b.add(int(2), int(2), -1);
        assert_eq!(b.to_sorted_vec(), vec![(int(1), int(1), 1)]);
    }
}
