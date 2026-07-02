//! Circuit nodes and their per-step delta kernels.
//!
//! Every node consumes its children's *deltas* for the current step and
//! produces its own output delta. The semantic contract, for every node kind:
//! integrating the deltas a node emits over any transaction sequence yields
//! exactly what the corresponding batch kernel in [`crate::eval::algebra`]
//! computes over the integrated inputs. The batch algebra is the oracle; the
//! property tests in `tests/dbsp.rs` check this equivalence step by step.
//!
//! This file currently holds the *linear* tier (SPEC §7): pure delta → delta
//! rules that need no state. Bilinear joins (compose, fork) and the non-linear
//! tier (distinct, semijoin, aggregation, …) land in follow-up milestones.

use crate::ast::CmpOp;
use crate::eval::interp::{compare_values, concat_values, mul_values, predicate};
use crate::eval::relation::{BTreeRelation, BinaryRelation};
use crate::eval::value::Value;
use crate::types::ty::SortId;
use crate::types::typed::Pred;
use std::collections::{BTreeMap, BTreeSet};

/// Index of a node in the circuit's arena. The arena is topologically ordered:
/// a node's children always have smaller indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub usize);

/// Identity of a base table: the per-sort diagonal, or one `(sort, field)`
/// columnar relation (§3: an entity is a family of field relations sharing an
/// ID key).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InputKey {
    /// The per-sort identity/diagonal (`Customer : CustID -> CustID`, §3.3).
    Identity(SortId),
    /// One field relation (`Customer:name : CustID -> Text`).
    Field(SortId, String),
}

/// A circuit operator. Children are named by [`NodeId`] and always precede the
/// node in the arena.
#[derive(Clone, Debug)]
pub enum Node {
    /// A base table; its delta is seeded from the transaction, never computed.
    Input(InputKey),
    /// A one-shot source for `Atom` / `ValueRef`: emits `{v -> v}` with weight
    /// +1 on the first step it participates in, and nothing ever after (its
    /// integral is constant).
    ConstSingleton { value: Value, fired: bool },
    /// `~R` (§3.1): converse. Linear.
    Inverse(NodeId),
    /// `R + S`: Z-set addition. Linear.
    Union(NodeId, NodeId),
    /// Filter the right column by a grounded predicate (`where > 30`). Linear.
    Filter(NodeId, Pred),
    /// `lhs in {set}`: coreflexive on the left key — `(k, v, w) -> (k, k, w)`
    /// when `v` is in the set. Linear.
    InRel(NodeId, BTreeSet<Value>),
    /// A constant relation over an identity input: rewrite the right column to
    /// a fixed value, `(id, id, w) -> (id, value, w)`. Linear.
    MapConst(NodeId, Value),
    /// `L . R` (§3.1): join L's right column against R's left, keep the outer
    /// columns. Bilinear: `δ(L·R) = δL·R' + I(L)·δR`, true O(Δ). `linv` is the
    /// node's private index of I(L) by *right* column (the probe side for the
    /// second term), maintained at commit.
    Compose {
        l: NodeId,
        r: NodeId,
        linv: BTreeRelation,
    },
    /// The co-keyed bilinear family (fork / `*` / `||` / binary comparison):
    /// pair L's and R's values per shared left key and combine with `f`.
    /// `δout = g(δL, R') + g(I(L), δR)`, true O(Δ). Both inputs are keyed on
    /// the shared left column, so the children's integrals are the only state
    /// needed — no private index.
    CoKeyed {
        l: NodeId,
        r: NodeId,
        f: CoKeyedFn,
    },
}

/// The value-combining function of a [`Node::CoKeyed`] node.
#[derive(Clone, Debug)]
pub enum CoKeyedFn {
    /// `L , R`: build a pair.
    Fork,
    /// `L * R`; `money` tags the result `Money` vs `Int` (from the node's type).
    Mul { money: bool },
    /// `L || R`: text concatenation.
    Concat,
    /// `L OP R`: coreflexive on the shared key when the comparison holds.
    Compare(CmpOp),
}

impl CoKeyedFn {
    /// Emit the output row for key `k` with co-keyed values `(b, c)` at
    /// combined weight `w` (zero-weight and failed comparisons emit nothing).
    fn apply(&self, out: &mut BTreeRelation, k: &Value, b: &Value, c: &Value, w: i64) {
        match self {
            CoKeyedFn::Fork => {
                out.add(k.clone(), Value::Pair(Box::new(b.clone()), Box::new(c.clone())), w)
            }
            CoKeyedFn::Mul { money } => out.add(k.clone(), mul_values(b, c, *money), w),
            CoKeyedFn::Concat => out.add(k.clone(), concat_values(b, c), w),
            CoKeyedFn::Compare(op) => {
                if compare_values(*op, b, c) {
                    out.add(k.clone(), k.clone(), w);
                }
            }
        }
    }
}

impl Node {
    /// The node's inputs, for topology checks.
    pub fn children(&self) -> Vec<NodeId> {
        match self {
            Node::Input(_) | Node::ConstSingleton { .. } => vec![],
            Node::Inverse(a) | Node::Filter(a, _) | Node::InRel(a, _) | Node::MapConst(a, _) => {
                vec![*a]
            }
            Node::Union(a, b) => vec![*a, *b],
            Node::Compose { l, r, .. } | Node::CoKeyed { l, r, .. } => vec![*l, *r],
        }
    }

    /// Compute this step's output delta from the children's deltas (and, for
    /// stateful nodes in later tiers, their pre-step integrals). Pure: node
    /// state is only mutated in the commit phase.
    pub(crate) fn compute(&self, ctx: &Ctx<'_>) -> BTreeRelation {
        match self {
            // Inputs are seeded by the step driver; never computed.
            Node::Input(_) => BTreeRelation::new(),
            Node::ConstSingleton { value, fired } => {
                let mut out = BTreeRelation::new();
                if !fired {
                    out.add(value.clone(), value.clone(), 1);
                }
                out
            }
            Node::Inverse(a) => {
                let mut out = BTreeRelation::new();
                for (l, r, w) in ctx.delta(*a).iter() {
                    out.add(r, l, w);
                }
                out
            }
            Node::Union(a, b) => {
                let mut out = BTreeRelation::new();
                for (l, r, w) in ctx.delta(*a).iter().chain(ctx.delta(*b).iter()) {
                    out.add(l, r, w);
                }
                out
            }
            Node::Filter(a, pred) => {
                let p = predicate(pred);
                let mut out = BTreeRelation::new();
                for (l, r, w) in ctx.delta(*a).iter() {
                    if p(&r) {
                        out.add(l, r, w);
                    }
                }
                out
            }
            Node::InRel(a, set) => {
                let mut out = BTreeRelation::new();
                for (l, r, w) in ctx.delta(*a).iter() {
                    if set.contains(&r) {
                        out.add(l.clone(), l, w);
                    }
                }
                out
            }
            Node::MapConst(a, value) => {
                let mut out = BTreeRelation::new();
                for (l, _, w) in ctx.delta(*a).iter() {
                    out.add(l, value.clone(), w);
                }
                out
            }
            Node::Compose { l, r, linv } => {
                let mut out = BTreeRelation::new();
                let r_new = Updated { old: ctx.integral(*r), delta: ctx.delta(*r) };
                // δL · R': new left rows joined against R as it will stand.
                for (a, b, w1) in ctx.delta(*l).iter() {
                    for (c, w2) in r_new.row(&b) {
                        out.add(a.clone(), c, w1 * w2);
                    }
                }
                // I(L) · δR: old left rows joined against R's changes, probed
                // through the right-column index.
                for (b, c, w2) in ctx.delta(*r).iter() {
                    for (a, w1) in linv.row(&b) {
                        out.add(a, c.clone(), w1 * w2);
                    }
                }
                out
            }
            Node::CoKeyed { l, r, f } => {
                let mut out = BTreeRelation::new();
                let r_new = Updated { old: ctx.integral(*r), delta: ctx.delta(*r) };
                // g(δL, R'): changed left values against R as it will stand.
                for k in ctx.delta(*l).domain() {
                    for (b, w1) in ctx.delta(*l).row(&k) {
                        for (c, w2) in r_new.row(&k) {
                            f.apply(&mut out, &k, &b, &c, w1 * w2);
                        }
                    }
                }
                // g(I(L), δR): old left values against R's changes.
                for k in ctx.delta(*r).domain() {
                    for (c, w2) in ctx.delta(*r).row(&k) {
                        for (b, w1) in ctx.integral(*l).row(&k) {
                            f.apply(&mut out, &k, &b, &c, w1 * w2);
                        }
                    }
                }
                out
            }
        }
    }

    /// Fold this step's committed deltas into any private node state. Called
    /// once per step, after all deltas are computed; `deltas` is the full
    /// per-node delta slice for the step.
    pub(crate) fn commit(&mut self, deltas: &[BTreeRelation]) {
        match self {
            Node::ConstSingleton { fired, .. } => *fired = true,
            Node::Compose { l, linv, .. } => {
                // Maintain linv = ~I(L).
                for (a, b, w) in deltas[l.0].iter() {
                    linv.add(b, a, w);
                }
            }
            _ => {}
        }
    }
}

/// Read-only view of a node's neighborhood during the compute phase: children's
/// deltas for this step and everyone's *pre-step* integrals.
pub(crate) struct Ctx<'a> {
    /// Deltas of nodes with smaller arena index (already computed this step).
    pub deltas: &'a [BTreeRelation],
    /// Pre-step integrals of every node.
    pub integrals: &'a [BTreeRelation],
}

impl Ctx<'_> {
    fn delta(&self, id: NodeId) -> &BTreeRelation {
        &self.deltas[id.0]
    }

    /// Pre-step integral of a child.
    fn integral(&self, id: NodeId) -> &BTreeRelation {
        &self.integrals[id.0]
    }
}

/// Read-only view of `I(X) + δX` — a relation as it will stand *after* this
/// step commits — used by kernels whose delta rule needs the post-step side
/// (compose's `δR·S'`, semijoin's membership test) while all stored integrals
/// still hold pre-step values.
pub struct Updated<'a> {
    pub old: &'a BTreeRelation,
    pub delta: &'a BTreeRelation,
}

impl Updated<'_> {
    /// Post-step weight of a pair.
    pub fn weight(&self, l: &Value, r: &Value) -> i64 {
        self.old.weight(l, r) + self.delta.weight(l, r)
    }

    /// Post-step row at `l`: merged, weights summed, zero entries pruned.
    pub fn row(&self, l: &Value) -> BTreeMap<Value, i64> {
        let mut merged: BTreeMap<Value, i64> = BTreeMap::new();
        for (r, w) in self.old.row(l).chain(self.delta.row(l)) {
            *merged.entry(r).or_insert(0) += w;
        }
        merged.retain(|_, w| *w != 0);
        merged
    }

    /// Post-step domain membership. This must merge-and-sum: a row whose old
    /// and delta weights cancel to zero counts as *absent* — the case semijoin
    /// flip detection depends on.
    pub fn has_left(&self, l: &Value) -> bool {
        !self.row(l).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: i64) -> Value {
        Value::Int(n)
    }

    fn rel(triples: &[(i64, i64, i64)]) -> BTreeRelation {
        BTreeRelation::from_triples(triples.iter().map(|&(l, r, w)| (int(l), int(r), w)))
    }

    #[test]
    fn updated_weight_sums_old_and_delta() {
        let old = rel(&[(1, 10, 2)]);
        let delta = rel(&[(1, 10, -1), (1, 11, 3)]);
        let u = Updated { old: &old, delta: &delta };
        assert_eq!(u.weight(&int(1), &int(10)), 1);
        assert_eq!(u.weight(&int(1), &int(11)), 3);
        assert_eq!(u.weight(&int(2), &int(10)), 0);
    }

    #[test]
    fn updated_row_prunes_cancelled_entries() {
        let old = rel(&[(1, 10, 1), (1, 11, 1)]);
        let delta = rel(&[(1, 10, -1)]);
        let u = Updated { old: &old, delta: &delta };
        let row = u.row(&int(1));
        assert_eq!(row.len(), 1);
        assert_eq!(row.get(&int(11)), Some(&1));
    }

    #[test]
    fn updated_has_left_treats_cancelled_row_as_absent() {
        // The single most bug-prone line of the design (semijoin membership
        // flips): old row fully cancelled by the delta must read as absent.
        let old = rel(&[(1, 10, 1)]);
        let delta = rel(&[(1, 10, -1)]);
        let u = Updated { old: &old, delta: &delta };
        assert!(!u.has_left(&int(1)));

        // ...and a row born in the delta alone must read as present.
        let empty = BTreeRelation::new();
        let birth = Updated { old: &empty, delta: &rel(&[(2, 20, 1)]) };
        assert!(birth.has_left(&int(2)));
    }

    #[test]
    fn updated_has_left_survives_partial_cancellation() {
        // Two entries, one cancelled: still present.
        let old = rel(&[(1, 10, 1), (1, 11, 1)]);
        let delta = rel(&[(1, 10, -1)]);
        let u = Updated { old: &old, delta: &delta };
        assert!(u.has_left(&int(1)));
    }
}
