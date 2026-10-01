//! Integrals: the maintained full value of a node (P-4).
//!
//! Two representations behind one read API:
//!
//! - [`Integral::Rel`], the general indexed Z-set ([`BTreeRelation`]).
//! - [`Integral::Col`], a **column** for a *functional, entity-keyed*
//!   relation: at most one value per key, weight exactly 1, keys the ids of one
//!   sort. Entity ids are per-sort, dense and never reused (`Engine::next_id`),
//!   so the key *is* an index: a probe is two array indexes and a write is a
//!   slot store, with no per-row allocation and no weights stored.
//!
//! The circuit picks a column for a node whose keys are proven to be one
//! sort's live ids ([`super::props::column_sort`]); it does not wait for a
//! proof that the node is functional. Any change a column cannot represent
//! (a second value at a key, a weight other than ±1, a key of another sort)
//! **demotes** it to a general relation first, so the choice decides speed
//! only, never results.

use super::batch::Batch;
use crate::eval::relation::{BTreeRelation, BinaryRelation};
use crate::eval::value::Value;
use crate::types::ty::SortId;
use std::collections::VecDeque;

const PAGE_BITS: u32 = 10;
const PAGE: usize = 1 << PAGE_BITS;

/// One page of 1024 consecutive ids. A slot holds `(key, value)`: the key is
/// kept so reads can hand out `&Value` for it like any relation does.
#[derive(Clone, Debug)]
struct Page {
    slots: Box<[Option<(Value, Value)>]>,
    live: u32,
}

impl Page {
    fn new() -> Box<Page> {
        Box::new(Page { slots: (0..PAGE).map(|_| None).collect(), live: 0 })
    }
}

/// A functional relation keyed by the ids of one sort, paged by `seq`.
///
/// Ids are never reused (they are persisted in the log), so a churning
/// workload leaves a dead prefix behind it: pages whose ids all died are
/// freed, and `base` moves past a freed prefix, so memory follows live pages
/// rather than ids ever minted.
#[derive(Clone, Debug)]
pub struct Column {
    sort: SortId,
    /// Page number of `pages[0]`.
    base: u64,
    pages: VecDeque<Option<Box<Page>>>,
    len: usize,
}

impl Column {
    pub fn new(sort: SortId) -> Column {
        Column { sort, base: 0, pages: VecDeque::new(), len: 0 }
    }

    fn seq(&self, key: &Value) -> Option<u64> {
        match key {
            Value::Id(s, n) if *s == self.sort => Some(*n),
            _ => None,
        }
    }

    fn slot(&self, seq: u64) -> Option<&(Value, Value)> {
        let page = (seq >> PAGE_BITS).checked_sub(self.base)?;
        let page = self.pages.get(page as usize)?.as_ref()?;
        page.slots[seq as usize & (PAGE - 1)].as_ref()
    }

    /// The value at `key`, if live.
    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.slot(self.seq(key)?).map(|(_, v)| v)
    }

    /// Apply `(left, right, weight)` if the column can represent the result;
    /// `false` (with nothing changed) if it cannot.
    fn try_add(&mut self, left: &Value, right: &Value, weight: i64) -> bool {
        let Some(seq) = self.seq(left) else { return false };
        match (self.slot(seq), weight) {
            (None, 1) => {
                self.insert(seq, left.clone(), right.clone());
                true
            }
            (Some((_, v)), -1) if v == right => {
                self.remove(seq);
                true
            }
            _ => false,
        }
    }

    fn insert(&mut self, seq: u64, key: Value, value: Value) {
        let p = seq >> PAGE_BITS;
        if self.pages.is_empty() {
            self.base = p;
        }
        while p < self.base {
            self.pages.push_front(None);
            self.base -= 1;
        }
        let at = (p - self.base) as usize;
        while self.pages.len() <= at {
            self.pages.push_back(None);
        }
        let page = self.pages[at].get_or_insert_with(Page::new);
        page.slots[seq as usize & (PAGE - 1)] = Some((key, value));
        page.live += 1;
        self.len += 1;
    }

    fn remove(&mut self, seq: u64) {
        let at = ((seq >> PAGE_BITS) - self.base) as usize;
        let page = self.pages[at].as_mut().expect("removing a live slot");
        page.slots[seq as usize & (PAGE - 1)] = None;
        page.live -= 1;
        self.len -= 1;
        if page.live == 0 {
            self.pages[at] = None;
            while matches!(self.pages.front(), Some(None)) {
                self.pages.pop_front();
                self.base += 1;
            }
            while matches!(self.pages.back(), Some(None)) {
                self.pages.pop_back();
            }
        }
    }

    /// Live `(key, value)` slots in `seq` order — which is `Value` order,
    /// since every key is an id of the one sort.
    fn slots(&self) -> impl Iterator<Item = &(Value, Value)> {
        self.pages.iter().flatten().flat_map(|p| p.slots.iter().flatten())
    }

    fn to_relation(&self) -> BTreeRelation {
        BTreeRelation::from_triples(self.slots().map(|(k, v)| (k.clone(), v.clone(), 1)))
    }
}

/// A node's integral; see the module docs.
#[derive(Clone, Debug)]
pub enum Integral {
    Rel(BTreeRelation),
    Col(Column),
}

impl Default for Integral {
    fn default() -> Integral {
        Integral::Rel(BTreeRelation::new())
    }
}

/// One of two iterators with the same item — how the two representations
/// share a read API without boxing.
pub enum Either<A, B> {
    A(A),
    B(B),
}

impl<T, A: Iterator<Item = T>, B: Iterator<Item = T>> Iterator for Either<A, B> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        match self {
            Either::A(a) => a.next(),
            Either::B(b) => b.next(),
        }
    }
}

impl Integral {
    /// An empty integral in the representation `column` names: a column for
    /// that sort's ids, else a general relation.
    pub fn empty(column: Option<SortId>) -> Integral {
        match column {
            Some(sort) => Integral::Col(Column::new(sort)),
            None => Integral::default(),
        }
    }

    pub fn is_column(&self) -> bool {
        matches!(self, Integral::Col(_))
    }

    /// Switch to the general representation, keeping the contents.
    fn demote(&mut self) -> &mut BTreeRelation {
        if let Integral::Col(c) = self {
            *self = Integral::Rel(c.to_relation());
        }
        match self {
            Integral::Rel(r) => r,
            Integral::Col(_) => unreachable!("just demoted"),
        }
    }

    pub fn add(&mut self, left: Value, right: Value, weight: i64) {
        if weight == 0 {
            return;
        }
        if let Integral::Col(c) = self
            && c.try_add(&left, &right, weight)
        {
            return;
        }
        self.demote().add(left, right, weight);
    }

    /// Fold one step's delta in. A column applies retractions before
    /// assertions: a consolidated batch orders a key's `−old` and `+new` by
    /// value, and the column holds one value per key at every point only if
    /// the old one leaves first.
    pub fn commit(&mut self, delta: &Batch) {
        match self {
            Integral::Rel(r) => {
                for (l, v, w) in delta.triples() {
                    r.add(l.clone(), v.clone(), w);
                }
            }
            Integral::Col(_) => {
                for sign in [true, false] {
                    for (l, v, w) in delta.triples().filter(|t| (t.2 < 0) == sign) {
                        self.add(l.clone(), v.clone(), w);
                    }
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Integral::Rel(r) => r.len(),
            Integral::Col(c) => c.len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn weight(&self, left: &Value, right: &Value) -> i64 {
        match self {
            Integral::Rel(r) => r.weight(left, right),
            Integral::Col(c) => (c.get(left) == Some(right)) as i64,
        }
    }

    pub fn has_left(&self, left: &Value) -> bool {
        match self {
            Integral::Rel(r) => r.has_left(left),
            Integral::Col(c) => c.get(left).is_some(),
        }
    }

    /// The `(right, weight)` entries at `left`, in right order.
    pub fn row_ref<'s>(&'s self, left: &Value) -> impl Iterator<Item = (&'s Value, i64)> + use<'s> {
        match self {
            Integral::Rel(r) => Either::A(r.row_ref(left)),
            Integral::Col(c) => Either::B(c.get(left).map(|v| (v, 1)).into_iter()),
        }
    }

    /// Every row, in `(left, right)` order, borrowed.
    pub fn triples(&self) -> impl Iterator<Item = (&Value, &Value, i64)> {
        match self {
            Integral::Rel(r) => Either::A(r.triples()),
            Integral::Col(c) => Either::B(c.slots().map(|(k, v)| (k, v, 1))),
        }
    }

    /// The distinct left keys, in order.
    pub fn keys(&self) -> impl Iterator<Item = &Value> {
        match self {
            Integral::Rel(r) => Either::A(r.keys()),
            Integral::Col(c) => Either::B(c.slots().map(|(k, _)| k)),
        }
    }

    pub fn to_sorted_vec(&self) -> Vec<(Value, Value, i64)> {
        self.triples().map(|(l, r, w)| (l.clone(), r.clone(), w)).collect()
    }

    /// The contents as a sorted batch (a backfill presents history this way).
    pub fn to_batch(&self) -> Batch {
        Batch::from_sorted(self.to_sorted_vec())
    }

    /// The contents as a general relation.
    pub fn to_relation(&self) -> BTreeRelation {
        match self {
            Integral::Rel(r) => r.clone(),
            Integral::Col(c) => c.to_relation(),
        }
    }
}

impl PartialEq for Integral {
    fn eq(&self, other: &Integral) -> bool {
        self.len() == other.len() && self.triples().eq(other.triples())
    }
}

impl PartialEq<BTreeRelation> for Integral {
    fn eq(&self, other: &BTreeRelation) -> bool {
        self.len() == other.len() && self.triples().eq(other.triples())
    }
}

impl PartialEq<Integral> for BTreeRelation {
    fn eq(&self, other: &Integral) -> bool {
        other == self
    }
}

impl BinaryRelation for Integral {
    fn add(&mut self, left: Value, right: Value, weight: i64) {
        Integral::add(self, left, right, weight)
    }

    fn weight(&self, left: &Value, right: &Value) -> i64 {
        Integral::weight(self, left, right)
    }

    fn len(&self) -> usize {
        Integral::len(self)
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (Value, Value, i64)> + '_> {
        Box::new(self.triples().map(|(l, r, w)| (l.clone(), r.clone(), w)))
    }

    fn row(&self, left: &Value) -> Box<dyn Iterator<Item = (Value, i64)> + '_> {
        Box::new(self.row_ref(left).map(|(r, w)| (r.clone(), w)).collect::<Vec<_>>().into_iter())
    }

    fn has_left(&self, left: &Value) -> bool {
        Integral::has_left(self, left)
    }

    fn domain(&self) -> Box<dyn Iterator<Item = Value> + '_> {
        Box::new(self.keys().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> Value {
        Value::Id(SortId(3), n)
    }

    #[test]
    fn column_holds_one_value_per_id_across_pages() {
        let mut c = Integral::empty(Some(SortId(3)));
        for n in [5, 2000, 3000, 1] {
            c.add(id(n), Value::Int(n as i64), 1);
        }
        assert!(c.is_column());
        assert_eq!(c.len(), 4);
        let keys: Vec<_> = c.keys().cloned().collect();
        assert_eq!(keys, vec![id(1), id(5), id(2000), id(3000)], "seq order is Value order");
        assert_eq!(c.weight(&id(2000), &Value::Int(2000)), 1);
        assert_eq!(c.weight(&id(2000), &Value::Int(1)), 0);
        // Update through a consolidated −old/+new batch whose +new sorts first.
        c.commit(&Batch::from_rows(vec![(id(5), Value::Int(5), -1), (id(5), Value::Int(0), 1)]));
        assert!(c.is_column());
        assert_eq!(c.row_ref(&id(5)).collect::<Vec<_>>(), vec![(&Value::Int(0), 1)]);
        // Retract everything: pages free and the column stays usable.
        for n in [0, 2000, 3000, 1] {
            let v = if n == 0 { 5 } else { n };
            c.add(id(v), Value::Int(n as i64), -1);
        }
        assert!(c.is_empty() && c.is_column());
        if let Integral::Col(col) = &c {
            assert!(col.pages.is_empty());
        }
        c.add(id(7), Value::Unit, 1);
        assert_eq!(c.to_sorted_vec(), vec![(id(7), Value::Unit, 1)]);
    }

    #[test]
    fn column_demotes_on_anything_it_cannot_hold() {
        let mut c = Integral::empty(Some(SortId(3)));
        c.add(id(1), Value::Int(1), 1);
        c.add(id(1), Value::Int(2), 1); // a second value at one key
        assert!(!c.is_column());
        assert_eq!(c.to_sorted_vec(), vec![(id(1), Value::Int(1), 1), (id(1), Value::Int(2), 1)]);

        let mut d = Integral::empty(Some(SortId(3)));
        d.add(id(1), Value::Int(1), -1); // a retraction of nothing
        assert!(!d.is_column());
        assert_eq!(d.weight(&id(1), &Value::Int(1)), -1);

        let mut e = Integral::empty(Some(SortId(3)));
        e.add(Value::Id(SortId(4), 1), Value::Unit, 1); // another sort's id
        assert!(!e.is_column());
        assert_eq!(e.len(), 1);
    }
}
