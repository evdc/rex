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
use crate::types::typed::{AggKind, Pred};
use std::collections::{BTreeMap, BTreeSet};

/// Index of a node in the circuit's arena. The arena is topologically ordered:
/// a node's children always have smaller indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub usize);

/// Identity of a base table: the per-sort diagonal, or one `(sort, field)`
/// columnar relation (§3: an entity is a family of field relations sharing an
/// ID key). Field names are interned, so the key is `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputKey {
    /// The per-sort identity/diagonal (`Customer : CustID -> CustID`, §3.3).
    Identity(SortId),
    /// One field relation (`Customer:name : CustID -> Text`).
    Field(SortId, crate::eval::intern::Sym),
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
    /// `L[R]` (§3.2): keep L's rows whose right column is a live key of R.
    /// Non-linear in R — the output depends on R only through the *membership*
    /// predicate `M(b) = R has a nonzero row at b`. Exact rule:
    /// `δout = δL ⋉ M'  +  I(L) ⋉ (M' − M)` — the second term fires only for
    /// keys whose membership *flips*, replaying the affected slice of I(L)
    /// through `linv` (I(L) indexed by right column). O(Δ + flipped slices).
    Semijoin {
        l: NodeId,
        r: NodeId,
        linv: BTreeRelation,
    },
    /// `L except R` (§6): `L − L[R]`, computed as `δL − δ(L[R])` using the
    /// semijoin kernel. Weight-safe by the same `L[R] ⊆ L` argument as batch.
    Antijoin {
        l: NodeId,
        r: NodeId,
        linv: BTreeRelation,
    },
    /// `L & R` (§3.1): elementwise `min` of weights. Non-linear but pointwise:
    /// for each pair touched by either delta, emit the change in
    /// `min(w_L, w_R)`. O(Δ).
    Intersect(NodeId, NodeId),
    /// `distinct L` (§3.1): clamp weights to {0,1}. Non-linear but pointwise:
    /// per touched pair, emit `clamp(new) − clamp(old)` ∈ {−1, 0, +1}. The
    /// input's integral is the only state needed. O(Δ).
    Distinct(NodeId),
    /// Aggregation (§5): a homomorphism from each key's image Z-set into a
    /// monoid, emitted with retract/assert deltas (old value out at −1, new
    /// value in at +1). Sum/Count/Avg fold deltas into per-key group state,
    /// O(Δ); Min/Max have no inverse and rescan each *touched* key's merged
    /// image — the recompute-per-group tier, never the whole DB.
    Aggregate {
        input: NodeId,
        kind: AggKind,
        /// Tag numeric results `Money` vs `Int` (from the argument's type).
        money: bool,
        st: BTreeMap<Value, KeyAgg>,
    },
    /// Placeholder inside a fix region's *inner* circuit, seeded by the region
    /// driver and never computed: either an imported outer stream (the Enter/δ₀
    /// side) or a member's feedback slot (the z⁻¹ edge). Which one it is lives
    /// in the owning `FixRegion`'s tables.
    FixInput,
    /// The *outer* face of one recursion-group member (§8): its delta is
    /// written by the fix-region driver in `Circuit::run` (the Exit side —
    /// converged fixpoint diffed against the previous step's). `imports` are
    /// the outer nodes feeding the region, duplicated here so `children()` —
    /// and with it the topological invariant and the dirty-cone skip — needs
    /// no access to the region table.
    FixOutput {
        region: usize,
        member: usize,
        imports: Vec<NodeId>,
    },
}

/// Per-key aggregate state: running group sums and the value last emitted
/// downstream (the one to retract on change).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyAgg {
    sum: i64,
    count: i64,
    last_out: Option<Value>,
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
            Node::Input(_) | Node::ConstSingleton { .. } | Node::FixInput => vec![],
            Node::FixOutput { imports, .. } => imports.clone(),
            Node::Inverse(a) | Node::Filter(a, _) | Node::InRel(a, _) | Node::MapConst(a, _) => {
                vec![*a]
            }
            Node::Union(a, b) | Node::Intersect(a, b) => vec![*a, *b],
            Node::Distinct(a) => vec![*a],
            Node::Aggregate { input, .. } => vec![*input],
            Node::Compose { l, r, .. }
            | Node::CoKeyed { l, r, .. }
            | Node::Semijoin { l, r, .. }
            | Node::Antijoin { l, r, .. } => vec![*l, *r],
        }
    }

    /// Forget all accumulated private state. Fix-region inner circuits are
    /// re-derived from scratch each outer step, so their nodes must start
    /// every activation as if never run.
    pub(crate) fn reset(&mut self) {
        match self {
            Node::ConstSingleton { fired, .. } => *fired = false,
            Node::Compose { linv, .. }
            | Node::Semijoin { linv, .. }
            | Node::Antijoin { linv, .. } => *linv = BTreeRelation::new(),
            Node::Aggregate { st, .. } => st.clear(),
            _ => {}
        }
    }

    /// Compute this step's output delta from the children's deltas and their
    /// pre-step integrals. Discipline: a node may mutate only its *private*
    /// state (`linv`, aggregate accumulators, `fired`), and only after fully
    /// deriving its delta from the pre-step values; the shared integrals are
    /// read-only until every node has computed.
    pub(crate) fn compute(&mut self, ctx: &Ctx<'_>) -> BTreeRelation {
        match self {
            // Inputs are seeded by the step driver; never computed.
            Node::Input(_) | Node::FixInput => BTreeRelation::new(),
            // Handled by the fix-region driver in `Circuit::run`, never here.
            Node::FixOutput { .. } => unreachable!("FixOutput is driven by the fix region"),
            Node::ConstSingleton { value, fired } => {
                let mut out = BTreeRelation::new();
                if !*fired {
                    out.add(value.clone(), value.clone(), 1);
                    *fired = true;
                }
                out
            }
            Node::Inverse(a) => {
                let mut out = BTreeRelation::new();
                for (l, r, w) in ctx.delta(*a).triples() {
                    out.add(r.clone(), l.clone(), w);
                }
                out
            }
            Node::Union(a, b) => {
                let mut out = BTreeRelation::new();
                for (l, r, w) in ctx.delta(*a).triples().chain(ctx.delta(*b).triples()) {
                    out.add(l.clone(), r.clone(), w);
                }
                out
            }
            Node::Filter(a, pred) => {
                let p = predicate(pred);
                let mut out = BTreeRelation::new();
                for (l, r, w) in ctx.delta(*a).triples() {
                    if p(r) {
                        out.add(l.clone(), r.clone(), w);
                    }
                }
                out
            }
            Node::InRel(a, set) => {
                let mut out = BTreeRelation::new();
                for (l, r, w) in ctx.delta(*a).triples() {
                    if set.contains(r) {
                        out.add(l.clone(), l.clone(), w);
                    }
                }
                out
            }
            Node::MapConst(a, value) => {
                let mut out = BTreeRelation::new();
                for (l, _, w) in ctx.delta(*a).triples() {
                    out.add(l.clone(), value.clone(), w);
                }
                out
            }
            Node::Compose { l, r, linv } => {
                let mut out = BTreeRelation::new();
                let r_new = Updated { old: ctx.integral(*r), delta: ctx.delta(*r) };
                // δL · R': new left rows joined against R as it will stand.
                for (a, b, w1) in ctx.delta(*l).triples() {
                    for (c, w2) in r_new.row(b) {
                        out.add(a.clone(), c.clone(), w1 * w2);
                    }
                }
                // I(L) · δR: old left rows joined against R's changes, probed
                // through the right-column index.
                for (b, c, w2) in ctx.delta(*r).triples() {
                    for (a, w1) in linv.row_ref(b) {
                        out.add(a.clone(), c.clone(), w1 * w2);
                    }
                }
                // Maintain linv = ~I(L) (after both terms read the old state).
                for (a, b, w) in ctx.delta(*l).triples() {
                    linv.add(b.clone(), a.clone(), w);
                }
                out
            }
            Node::CoKeyed { l, r, f } => {
                let mut out = BTreeRelation::new();
                let r_new = Updated { old: ctx.integral(*r), delta: ctx.delta(*r) };
                // g(δL, R'): changed left values against R as it will stand.
                // R's merged row is materialized once per key, not per pair.
                for (k, lrow) in ctx.delta(*l).rows() {
                    let r_row: Vec<(&Value, i64)> = r_new.row(k).collect();
                    for (b, w1) in lrow {
                        for &(c, w2) in &r_row {
                            f.apply(&mut out, k, b, c, w1 * w2);
                        }
                    }
                }
                // g(I(L), δR): old left values against R's changes.
                for (k, drow) in ctx.delta(*r).rows() {
                    let Some(lrow) = ctx.integral(*l).row_map(k) else { continue };
                    for (c, w2) in drow {
                        for (b, w1) in lrow {
                            f.apply(&mut out, k, b, c, w1 * w2);
                        }
                    }
                }
                out
            }
            Node::Semijoin { l, r, linv } => {
                let out =
                    semijoin_delta(ctx.delta(*l), linv, ctx.integral(*r), ctx.delta(*r));
                for (a, b, w) in ctx.delta(*l).triples() {
                    linv.add(b.clone(), a.clone(), w);
                }
                out
            }
            Node::Antijoin { l, r, linv } => {
                // δ(L − L[R]) = δL − δ(L[R]).
                let sj = semijoin_delta(ctx.delta(*l), linv, ctx.integral(*r), ctx.delta(*r));
                let mut out = BTreeRelation::new();
                for (a, b, w) in ctx.delta(*l).triples() {
                    out.add(a.clone(), b.clone(), w);
                }
                for (a, b, w) in sj.triples() {
                    out.add(a.clone(), b.clone(), -w);
                }
                for (a, b, w) in ctx.delta(*l).triples() {
                    linv.add(b.clone(), a.clone(), w);
                }
                out
            }
            Node::Intersect(a, b) => {
                // Pointwise: only pairs touched by either delta can change.
                let (da, db) = (ctx.delta(*a), ctx.delta(*b));
                let (ia, ib) = (ctx.integral(*a), ctx.integral(*b));
                let mut touched: BTreeSet<(&Value, &Value)> = BTreeSet::new();
                for (l, r, _) in da.triples().chain(db.triples()) {
                    touched.insert((l, r));
                }
                let mut out = BTreeRelation::new();
                for (l, r) in touched {
                    let old = ia.weight(l, r).min(ib.weight(l, r));
                    let new = (ia.weight(l, r) + da.weight(l, r))
                        .min(ib.weight(l, r) + db.weight(l, r));
                    out.add(l.clone(), r.clone(), new - old);
                }
                out
            }
            Node::Distinct(a) => {
                // Pointwise: emit clamp(new) − clamp(old) per touched pair.
                // The input's integral is the state; no private copy needed.
                let clamp = |w: i64| (w > 0) as i64;
                let old = ctx.integral(*a);
                let mut out = BTreeRelation::new();
                for (l, r, dw) in ctx.delta(*a).triples() {
                    let before = old.weight(l, r);
                    out.add(l.clone(), r.clone(), clamp(before + dw) - clamp(before));
                }
                out
            }
            Node::Aggregate { input, kind, money, st } => {
                let delta = ctx.delta(*input);
                let upd = Updated { old: ctx.integral(*input), delta };
                let mut out = BTreeRelation::new();
                for (k, drow) in delta.rows() {
                    let prev = st.get(k).cloned().unwrap_or_default();
                    // Group tier: fold the delta into the running (sum, count).
                    let mut sum = prev.sum;
                    let mut count = prev.count;
                    for (v, w) in drow {
                        sum += v.as_i64().unwrap_or(0) * w;
                        count += w;
                    }
                    // Presence must match batch exactly: a key is emitted iff
                    // its merged image has any nonzero-weight entry (mixed-sign
                    // groups can be present with count == 0).
                    let present = upd.has_left(k);
                    let new_out = if !present {
                        None
                    } else {
                        match kind {
                            AggKind::Sum => Some(mk_num(sum, *money)),
                            AggKind::Count => Some(Value::Int(count)),
                            AggKind::Avg => {
                                // Mirrors the batch kernel's `continue` on an
                                // empty count; always Money, i64 division.
                                if count == 0 { None } else { Some(Value::Money(sum / count)) }
                            }
                            // No inverse (SPEC §5): rescan this key's merged
                            // image, mirroring the batch accumulator.
                            AggKind::Min => upd
                                .row(k)
                                .filter(|&(_, w)| w > 0)
                                .map(|(v, _)| v.as_i64().unwrap_or(0))
                                .min()
                                .map(|m| mk_num(m, *money)),
                            AggKind::Max => upd
                                .row(k)
                                .filter(|&(_, w)| w > 0)
                                .map(|(v, _)| v.as_i64().unwrap_or(0))
                                .max()
                                .map(|m| mk_num(m, *money)),
                        }
                    };
                    // Retract/assert: batch output rows always carry weight 1.
                    if new_out != prev.last_out {
                        if let Some(o) = &prev.last_out {
                            out.add(k.clone(), o.clone(), -1);
                        }
                        if let Some(n) = &new_out {
                            out.add(k.clone(), n.clone(), 1);
                        }
                    }
                    if !present {
                        // Every entry cancelled, so the folded sums are 0 too:
                        // dropping the key keeps state proportional to live keys.
                        st.remove(k);
                    } else {
                        st.insert(k.clone(), KeyAgg { sum, count, last_out: new_out });
                    }
                }
                out
            }
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
    /// Backfill boundary: nodes below this index present their full integral
    /// *as* their delta (see [`Ctx::delta`]), so their integral must read as
    /// empty here (otherwise the history would be counted twice). 0 during a
    /// normal step.
    pub floor: usize,
}

static EMPTY: std::sync::LazyLock<BTreeRelation> = std::sync::LazyLock::new(BTreeRelation::new);

impl Ctx<'_> {
    /// This step's delta of a child. During a backfill, below-floor nodes
    /// present their full integral *as* their delta — by reference, so the
    /// history replay copies nothing.
    pub(crate) fn delta(&self, id: NodeId) -> &BTreeRelation {
        if id.0 < self.floor { &self.integrals[id.0] } else { &self.deltas[id.0] }
    }

    /// Pre-step integral of a child (empty for nodes being replayed as deltas
    /// during a backfill).
    pub(crate) fn integral(&self, id: NodeId) -> &BTreeRelation {
        if id.0 < self.floor { &EMPTY } else { &self.integrals[id.0] }
    }
}

/// The semijoin delta rule, shared by [`Node::Semijoin`] and [`Node::Antijoin`]:
/// `δout = δL ⋉ M'  +  I(L) ⋉ (M' − M)` where `M(b)` is R's left-domain
/// membership. `linv` is pre-step `~I(L)`; the caller folds `~δL` in afterward.
fn semijoin_delta(
    delta_l: &BTreeRelation,
    linv: &BTreeRelation,
    r_old: &BTreeRelation,
    r_delta: &BTreeRelation,
) -> BTreeRelation {
    let r_new = Updated { old: r_old, delta: r_delta };
    let mut out = BTreeRelation::new();
    // δL ⋉ M': new left rows kept iff their right key is live after the step.
    for (a, b, w) in delta_l.triples() {
        if r_new.has_left(b) {
            out.add(a.clone(), b.clone(), w);
        }
    }
    // I(L) ⋉ (M' − M): keys whose membership flips replay their I(L) slice.
    for b in r_delta.keys() {
        let before = r_old.has_left(b);
        let after = r_new.has_left(b);
        if before != after {
            let sign = if after { 1 } else { -1 };
            for (a, w) in linv.row_ref(b) {
                out.add(a.clone(), b.clone(), sign * w);
            }
        }
    }
    out
}

/// Tag a numeric aggregate result, mirroring the batch kernel's `mk`.
fn mk_num(n: i64, money: bool) -> Value {
    if money { Value::Money(n) } else { Value::Int(n) }
}

/// Read-only view of `I(X) + δX` — a relation as it will stand *after* this
/// step commits — used by kernels whose delta rule needs the post-step side
/// (compose's `δR·S'`, semijoin's membership test) while all stored integrals
/// still hold pre-step values.
pub struct Updated<'a> {
    pub old: &'a BTreeRelation,
    pub delta: &'a BTreeRelation,
}

impl<'a> Updated<'a> {
    /// Post-step weight of a pair.
    pub fn weight(&self, l: &Value, r: &Value) -> i64 {
        self.old.weight(l, r) + self.delta.weight(l, r)
    }

    /// Post-step row at `l`: a sorted merge of the old and delta rows, weights
    /// summed, zero entries skipped — single pass, no allocation.
    pub fn row(&self, l: &Value) -> impl Iterator<Item = (&'a Value, i64)> + use<'a> {
        MergeSum {
            a: self.old.row_ref(l).peekable(),
            b: self.delta.row_ref(l).peekable(),
        }
    }

    /// Post-step domain membership. This must merge-and-sum: a row whose old
    /// and delta weights cancel to zero counts as *absent* — the case semijoin
    /// flip detection depends on. Short-circuits on the first surviving entry.
    pub fn has_left(&self, l: &Value) -> bool {
        self.row(l).next().is_some()
    }
}

/// Sorted merge-join of two `(key, weight)` streams, summing weights on equal
/// keys and skipping entries that cancel to zero. Both inputs must be sorted
/// by key (BTree row order guarantees it).
struct MergeSum<K: Ord, A: Iterator<Item = (K, i64)>, B: Iterator<Item = (K, i64)>> {
    a: std::iter::Peekable<A>,
    b: std::iter::Peekable<B>,
}

impl<K: Ord, A: Iterator<Item = (K, i64)>, B: Iterator<Item = (K, i64)>> Iterator
    for MergeSum<K, A, B>
{
    type Item = (K, i64);

    fn next(&mut self) -> Option<(K, i64)> {
        loop {
            let ord = match (self.a.peek(), self.b.peek()) {
                (None, None) => return None,
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some((ka, _)), Some((kb, _))) => ka.cmp(kb),
            };
            match ord {
                std::cmp::Ordering::Less => return self.a.next(),
                std::cmp::Ordering::Greater => return self.b.next(),
                std::cmp::Ordering::Equal => {
                    let (k, wa) = self.a.next().expect("peeked");
                    let (_, wb) = self.b.next().expect("peeked");
                    // A cancelled entry counts as absent — keep scanning.
                    if wa + wb != 0 {
                        return Some((k, wa + wb));
                    }
                }
            }
        }
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
        let row: Vec<(Value, i64)> = u.row(&int(1)).map(|(v, w)| (v.clone(), w)).collect();
        assert_eq!(row, vec![(int(11), 1)]);
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
