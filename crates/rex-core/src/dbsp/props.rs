//! Static properties of circuit nodes, read off their construction: which
//! sort's live ids bound a node's keys (`anchor`) or values, and whether it
//! holds at most one weight-1 row per key (`functional`, P-4a). Lowering's
//! rewrites (P-1, P-2b) are justified by them, and the circuit picks each
//! integral's representation by them (P-4b, [`super::integral`]).
//!
//! Every property here rests on the engine's base invariant (see
//! `Engine::debug_assert_base_invariant`) and on each kernel being exact.

use super::circuit::Circuit;
use super::node::{CoKeyedFn, InputKey, Node, NodeId};
use crate::types::ty::SortId;

/// The sort whose live ids bound `id`'s left keys at every step boundary, if
/// the node's construction guarantees one.
pub fn anchor(c: &Circuit, id: NodeId) -> Option<SortId> {
    match c.node(id) {
        Node::Input(InputKey::Identity(s) | InputKey::Field(s, _)) => Some(*s),
        Node::MapConst(a, _)
        | Node::FilterMap(a, _)
        | Node::Filter(a, _)
        | Node::InRel(a, _)
        | Node::Proj(a, _)
        | Node::Distinct(a) => anchor(c, *a),
        Node::Compose { l, .. } | Node::Semijoin { l, .. } | Node::Antijoin { l, .. } => anchor(c, *l),
        // Co-keyed output needs a key on both sides; either side bounds it.
        Node::CoKeyed { l, r, .. } => anchor(c, *l).or_else(|| anchor(c, *r)),
        // Union adds weights and intersect takes their `min` (−1 against an
        // absent row is −1), so a key from either side can survive.
        Node::Union(a, b) | Node::Intersect(a, b) => anchor(c, *a).filter(|s| anchor(c, *b) == Some(*s)),
        _ => None,
    }
}

/// The sort whose live ids bound `id`'s *values* at every step boundary.
pub(crate) fn node_values_anchor(c: &Circuit, id: NodeId) -> Option<SortId> {
    match c.node(id) {
        Node::Input(InputKey::Identity(s)) => Some(*s),
        // Each emits the key itself as the value, and keys are anchored.
        Node::InRel(..) => anchor(c, id),
        Node::FilterMap(_, f) if f.is_coreflexive() => anchor(c, id),
        Node::CoKeyed { f: CoKeyedFn::Compare(_), .. } => anchor(c, id),
        // Values of the inverse are the input's keys.
        Node::Inverse(a) => anchor(c, *a),
        // Each keeps a subset of one child's rows (or of its values).
        Node::Filter(a, _) | Node::Distinct(a) => node_values_anchor(c, *a),
        Node::Semijoin { l, .. } | Node::Antijoin { l, .. } => node_values_anchor(c, *l),
        Node::Compose { r, .. } => node_values_anchor(c, *r),
        Node::Union(a, b) | Node::Intersect(a, b) => {
            node_values_anchor(c, *a).filter(|s| node_values_anchor(c, *b) == Some(*s))
        }
        _ => None,
    }
}

/// Whether `id` holds at most one row per left key, at weight 1.
///
/// Multiplicity inference (P-4a). Base inputs are functional by the engine's
/// base invariant; a singleton and an aggregate emit one weight-1 row per key.
/// A per-row map, a filter or a subset of a functional relation is one; so is
/// `L . R` or a co-keyed op of two (`1·1` rows, one pairing per key). Union,
/// inverse and intersect are not: they can put two values at one key.
pub fn functional(c: &Circuit, id: NodeId) -> bool {
    match c.node(id) {
        Node::Input(_) | Node::ConstSingleton { .. } | Node::Aggregate { .. } => true,
        Node::MapConst(a, _)
        | Node::FilterMap(a, _)
        | Node::Filter(a, _)
        | Node::InRel(a, _)
        | Node::Proj(a, _)
        | Node::Distinct(a) => functional(c, *a),
        Node::Semijoin { l, .. } | Node::Antijoin { l, .. } => functional(c, *l),
        Node::Compose { l, r, .. } | Node::CoKeyed { l, r, .. } => functional(c, *l) && functional(c, *r),
        _ => false,
    }
}

pub(crate) fn node_coreflexive_on(c: &Circuit, id: NodeId, sort: SortId) -> bool {
    anchor(c, id) == Some(sort)
        && match c.node(id) {
            Node::Input(InputKey::Identity(_)) => true,
            Node::FilterMap(a, f) => f.is_coreflexive() && functional(c, *a),
            Node::InRel(a, _) => functional(c, *a),
            // A subset of a weight-1 coreflexive's rows, at their weights
            // (P-2b): what every `E where not …` lowers to.
            Node::Semijoin { l, .. } | Node::Antijoin { l, .. } => node_coreflexive_on(c, *l, sort),
            // `(k, k)·(k, k)` at weight 1·1: the intersection of two.
            Node::Compose { l, r, .. } => node_coreflexive_on(c, *l, sort) && node_coreflexive_on(c, *r, sort),
            _ => false,
        }
}

/// The sort whose ids key a column integral for `id` (P-4b), or `None` for
/// the general representation: any node whose keys are proven to be one
/// sort's ids. Functionality is *not* required up front. A column demotes
/// itself the first time it would hold two values at a key, so a wrong guess
/// costs one conversion, while demanding a proof would leave out what is
/// functional only in practice: the union of a `match`'s disjoint branches
/// (TodoMVC's `visible`) and every field read through it.
pub fn column_sort(c: &Circuit, id: NodeId) -> Option<SortId> {
    anchor(c, id)
}
