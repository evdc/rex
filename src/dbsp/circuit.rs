//! The circuit: an arena of operator nodes plus the integrated output of each.
//!
//! Invariant: the arena is **topologically ordered** — every node's children
//! have smaller indices (lowering builds children first, and view references
//! point backward). One `step` is therefore a single forward pass:
//!
//! 1. **Compute.** Seed input-node deltas from the transaction, then walk the
//!    arena in order; each node derives its output delta from its children's
//!    deltas and their pre-step integrals, then updates its own *private*
//!    state. All shared integrals hold *pre-step* values throughout this phase.
//! 2. **Commit.** Fold every node's delta into its integral.

use super::node::{Ctx, InputKey, Node, NodeId};
use crate::eval::relation::{BTreeRelation, BinaryRelation};
use crate::eval::value::Value;
use std::collections::HashMap;

/// One atomic batch of base-table changes. A single `new E { .. }` is one
/// transaction: the identity row plus every field row at the same fresh id
/// (never N independent inserts, §4).
#[derive(Clone, Debug, Default)]
pub struct Transaction {
    pub deltas: Vec<(InputKey, Value, Value, i64)>,
}

impl Transaction {
    pub fn new() -> Transaction {
        Transaction::default()
    }

    pub fn push(&mut self, key: InputKey, left: Value, right: Value, weight: i64) {
        self.deltas.push((key, left, right, weight));
    }
}

/// The per-step output: each named view's delta for this step.
#[derive(Debug, Default)]
pub struct StepResult {
    pub view_deltas: HashMap<String, BTreeRelation>,
}

#[derive(Debug, Default)]
pub struct Circuit {
    nodes: Vec<Node>,
    /// `I(output)` of every node, parallel to `nodes`.
    integrals: Vec<BTreeRelation>,
    /// Base-table dedup: every mention of the same field must be the same node.
    inputs: HashMap<InputKey, NodeId>,
    /// View name -> producing node.
    outputs: HashMap<String, NodeId>,
}

impl Circuit {
    pub fn new() -> Circuit {
        Circuit::default()
    }

    /// Append a node to the arena. Children must already exist (topological
    /// order is the circuit's core invariant).
    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len());
        debug_assert!(
            node.children().iter().all(|c| c.0 < id.0),
            "arena must stay topologically ordered"
        );
        self.nodes.push(node);
        self.integrals.push(BTreeRelation::new());
        id
    }

    /// Get or create the input node for a base table. Called both by lowering
    /// (a view mentions the field) and by data ingestion (a `new` writes it),
    /// in either order.
    pub fn input(&mut self, key: InputKey) -> NodeId {
        if let Some(id) = self.inputs.get(&key) {
            return *id;
        }
        let id = self.add_node(Node::Input(key.clone()));
        self.inputs.insert(key, id);
        id
    }

    /// Register `node` as the producer of the named view.
    pub fn set_output(&mut self, name: &str, node: NodeId) {
        self.outputs.insert(name.to_string(), node);
    }

    pub fn output(&self, name: &str) -> Option<NodeId> {
        self.outputs.get(name).copied()
    }

    /// The integrated (full) contents of a named view.
    pub fn view(&self, name: &str) -> Option<&BTreeRelation> {
        self.outputs.get(name).map(|id| &self.integrals[id.0])
    }

    /// The integrated output of any node (tests, backfill, retraction reads).
    pub fn integral(&self, id: NodeId) -> &BTreeRelation {
        &self.integrals[id.0]
    }

    /// The integrated contents of a base table, if it has been touched.
    pub fn input_integral(&self, key: &InputKey) -> Option<&BTreeRelation> {
        self.inputs.get(key).map(|id| &self.integrals[id.0])
    }

    /// Number of nodes in the arena — the backfill mark to take *before*
    /// lowering a new view.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// The base tables the circuit knows about.
    pub fn input_keys(&self) -> impl Iterator<Item = &InputKey> {
        self.inputs.keys()
    }

    /// Run one transaction through the circuit and return each view's delta.
    pub fn step(&mut self, tx: &Transaction) -> StepResult {
        // Ensure every targeted base table exists *before* sizing the delta
        // vector, so data arriving ahead of any view that reads it is kept.
        for (key, _, _, _) in &tx.deltas {
            self.input(key.clone());
        }

        let mut deltas: Vec<BTreeRelation> = vec![BTreeRelation::new(); self.nodes.len()];
        for (key, l, r, w) in &tx.deltas {
            let id = self.inputs[key];
            deltas[id.0].add(l.clone(), r.clone(), *w);
        }
        self.run(0, deltas)
    }

    /// Evaluate the freshly appended node suffix `from..` over the data already
    /// in the circuit (a new `let` over existing base tables). Every node below
    /// `from` presents its full integral as one first delta — and reads as an
    /// *empty* integral, so the history is seen exactly once. Because every
    /// delta rule is exact, δ-from-empty equals batch evaluation by
    /// construction. Pre-existing nodes are neither recomputed nor recommitted.
    pub fn backfill(&mut self, from: usize) -> StepResult {
        let deltas: Vec<BTreeRelation> = (0..self.nodes.len())
            .map(|j| if j < from { self.integrals[j].clone() } else { BTreeRelation::new() })
            .collect();
        self.run(from, deltas)
    }

    /// The shared driver: compute nodes `floor..` in topological order, commit
    /// their deltas, and report deltas for views produced at or above `floor`.
    fn run(&mut self, floor: usize, mut deltas: Vec<BTreeRelation>) -> StepResult {
        // Phase 1: compute. Shared integrals stay pre-step throughout; nodes
        // below `floor` keep their seeded deltas and read as empty integrals.
        for i in floor..self.nodes.len() {
            if matches!(self.nodes[i], Node::Input(_)) {
                continue; // seeded, never computed
            }
            let (prev, rest) = deltas.split_at_mut(i);
            let ctx = Ctx { deltas: prev, integrals: &self.integrals, floor };
            rest[0] = self.nodes[i].compute(&ctx);
        }

        // Phase 2: commit — fold computed deltas into their nodes' integrals.
        for (i, delta) in deltas.iter().enumerate().skip(floor) {
            for (l, r, w) in delta.iter() {
                self.integrals[i].add(l, r, w);
            }
        }

        let mut result = StepResult::default();
        for (name, id) in &self.outputs {
            if id.0 >= floor {
                result.view_deltas.insert(name.clone(), deltas[id.0].clone());
            }
        }
        result
    }
}
