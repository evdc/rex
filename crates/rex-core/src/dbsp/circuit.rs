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
//! 2. **Commit.** Fold each node's delta into its integral — for the nodes
//!    that keep one (P-2): base inputs, views, fix-region members, and any
//!    node a parent's kernel reads whole ([`Node::integral_reads`]). A chain
//!    of linear nodes feeding only other linear nodes keeps no state at all.
//!
//! Deltas are flat sorted [`Batch`]es (P-3); integrals are [`Integral`]s, a
//! column for a node keyed by one sort's ids and a general indexed relation
//! otherwise (P-4, chosen by [`column_sort`] when the node starts keeping one).
//!
//! Equal nodes are one node ([`Circuit::add_node`], P-2b). Adding views one at
//! a time means a later view can reuse a node that has already stepped without
//! an integral; its backfill then recomputes that node from its children's
//! history ([`Circuit::backfill`]).

use super::batch::{Batch, BatchBuilder, Row};
use super::integral::Integral;
use super::node::{Ctx, InputKey, Node, NodeId};
use super::props::column_sort;
use crate::eval::value::Value;
use crate::types::typed::Total;
use std::collections::HashMap;

/// One atomic batch of base-table changes. A single `new E { .. }` is one
/// transaction: the identity row plus every field row at the same fresh id
/// (never N independent inserts, §4).
#[derive(Clone, Debug, Default)]
pub struct Transaction {
    pub deltas: Vec<(InputKey, Value, Value, i64)>,
    /// Every left key (entity id) some delta here names, so a later op in the
    /// same transaction can tell in O(1) whether it must compose with pending
    /// writes at all — without it, retracting N rows scans `deltas` N times.
    touched: std::collections::HashSet<Value>,
}

impl Transaction {
    pub fn new() -> Transaction {
        Transaction::default()
    }

    pub fn push(&mut self, key: InputKey, left: Value, right: Value, weight: i64) {
        // A row's deltas are pushed together, so the last one is the usual hit.
        if self.deltas.last().is_none_or(|(_, l, _, _)| *l != left) {
            self.touched.insert(left.clone());
        }
        self.deltas.push((key, left, right, weight));
    }

    /// Whether any delta so far has `left` as its left key.
    pub fn touches(&self, left: &Value) -> bool {
        self.touched.contains(left)
    }
}

/// The per-step output: each named view's delta for this step.
#[derive(Debug, Default)]
pub struct StepResult {
    pub view_deltas: HashMap<String, Batch>,
}

#[derive(Debug, Default)]
pub struct Circuit {
    nodes: Vec<Node>,
    /// `I(output)` of every node, parallel to `nodes`; stays empty for a node
    /// that does not `keep` one.
    integrals: Vec<Integral>,
    /// Whether each node's integral is maintained (P-2): demand from parents,
    /// view registration, or being an input. Parallel to `nodes`.
    keep: Vec<bool>,
    /// How many nodes have been through a step. A node below this mark that
    /// did not keep an integral has no record of its history; it can serve new
    /// parents only by being recomputed (`Node::is_replayable`, `revive`).
    stepped: usize,
    /// Stepped nodes that started keeping an integral after the fact (P-2b).
    /// Their integral is rebuilt by the next [`backfill`](Self::backfill),
    /// which must come before any step.
    revive: Vec<NodeId>,
    /// Base-table dedup: every mention of the same field must be the same node.
    inputs: HashMap<InputKey, NodeId>,
    /// Every other node by [`Node::share_key`] (P-2b), so a subterm that
    /// lowering reaches twice — within one view or across views — is one node.
    shared: HashMap<String, NodeId>,
    /// View name -> producing node.
    outputs: HashMap<String, NodeId>,
    /// Nested fixpoint subcircuits (§8), indexed by `Node::FixOutput.region`.
    fixes: Vec<FixRegion>,
}

/// One recursion group's nested circuit (§8, DBSP Enter/Exit style). The outer
/// arena stays strictly topological; the cycle lives entirely in here, driven
/// to its least fixpoint within each outer step:
///
/// - **Enter (δ₀):** each import's full post-step value is presented to the
///   inner circuit once, as the iteration-0 delta of its `FixInput`.
/// - **Iterate (z⁻¹ + distinct):** run the inner arena one pass per iteration;
///   the forced `distinct` clamp of each member's body, minus what the member's
///   feedback slot has already integrated, is fed back as the next iteration's
///   delta. Inner integrals persist across iterations, so each pass processes
///   only new tuples — semi-naive evaluation for free.
/// - **Exit (∫):** the converged fixpoint, diffed against the previous outer
///   step's (`prev`), is each member's outer delta. Recomputing the fixpoint
///   per step and diffing makes retractions correct by construction; fully
///   incremental nested deltas are future work.
#[derive(Debug)]
pub struct FixRegion {
    inner: Circuit,
    /// Outer nodes feeding the region (Enter sources), parallel to `import_nodes`.
    imports: Vec<NodeId>,
    /// Inner `FixInput` placeholder per import.
    import_nodes: Vec<NodeId>,
    /// Inner `FixInput` feedback slot per member (the z⁻¹ edge).
    rec_inputs: Vec<NodeId>,
    /// Inner node producing each member's body.
    member_outs: Vec<NodeId>,
    /// Last converged fixpoint per member (Exit state).
    prev: Vec<Batch>,
    /// Whether the region has ever been evaluated. A region with no imports
    /// (a constant-only body) has no children to wake it, so — like an
    /// unfired `ConstSingleton` — it must fire once regardless of quiescence.
    fired: bool,
}

/// Z-set subtraction `a − b` (core-only, §6).
fn zsub(a: &Batch, b: &Batch) -> Batch {
    let mut rows: Vec<Row> = a.as_slice().to_vec();
    rows.extend(b.triples().map(|(l, r, w)| (l.clone(), r.clone(), -w)));
    Batch::from_rows(rows)
}

impl FixRegion {
    /// Evaluate the region to its least fixpoint for this outer step; returns
    /// each member's *outer* delta. `ctx` reads the imports' outer deltas and
    /// pre-step integrals (under a backfill floor this still yields exactly
    /// the full current value).
    fn evaluate(&mut self, ctx: &Ctx<'_>) -> Vec<Batch> {
        self.fired = true;
        self.inner.reset_state();
        // Enter (δ₀): each import's full post-step value at iteration 0.
        let mut d = vec![Batch::new(); self.inner.nodes.len()];
        for (k, outer) in self.imports.iter().enumerate() {
            let mut full = ctx.integral(*outer).to_sorted_vec();
            full.extend_from_slice(ctx.delta(*outer).as_slice());
            d[self.import_nodes[k].0] = Batch::from_rows(full);
        }
        loop {
            let pass = self.inner.iterate(d);
            // The knot: the forced distinct, applied incrementally — the same
            // clamp(new) − clamp(old) rule as `Node::Distinct`, over only this
            // pass's member delta. The feedback slot telescopes: its integral
            // is distinct(member_out) as of the previous pass, so the clamp
            // *changes* are exactly what it hasn't integrated yet.
            d = vec![Batch::new(); self.inner.nodes.len()];
            let mut changed = false;
            let clamp = |w: i64| (w > 0) as i64;
            for m in 0..self.member_outs.len() {
                let out_int = self.inner.integral(self.member_outs[m]);
                let mut fb = BatchBuilder::new();
                for (l, r, dw) in pass[self.member_outs[m].0].triples() {
                    let after = out_int.weight(l, r); // post-commit
                    fb.push(l.clone(), r.clone(), clamp(after) - clamp(after - dw));
                }
                let fb = fb.finish();
                if !fb.is_empty() {
                    changed = true;
                    d[self.rec_inputs[m].0] = fb;
                }
            }
            if !changed {
                break;
            }
        }
        // Exit: diff the converged fixpoint against the previous outer step's.
        (0..self.member_outs.len())
            .map(|m| {
                let result = self.inner.integral(self.rec_inputs[m]).to_batch();
                let delta = zsub(&result, &self.prev[m]);
                self.prev[m] = result;
                delta
            })
            .collect()
    }
}

impl Circuit {
    pub fn new() -> Circuit {
        Circuit::default()
    }

    /// Append a node to the arena, or return an existing node that computes
    /// the same relation from the same children (P-2b). Children must already
    /// exist (topological order is the circuit's core invariant).
    ///
    /// An existing node is shared only if it can present its history to a new
    /// view's backfill (`replayable`).
    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len());
        debug_assert!(
            node.children().iter().all(|c| c.0 < id.0),
            "arena must stay topologically ordered"
        );
        let key = node.share_key();
        if let Some(&old) = key.as_ref().and_then(|k| self.shared.get(k))
            && self.replayable(old)
        {
            return old;
        }
        if let Some(key) = key {
            self.shared.insert(key, id);
        }
        let reads = node.integral_reads();
        let keep = matches!(node, Node::Input(_) | Node::FixInput);
        self.keep.push(false);
        self.nodes.push(node);
        self.integrals.push(Integral::default());
        if keep {
            self.keep_integral(id);
        }
        for child in reads {
            self.keep_integral(child);
        }
        id
    }

    /// Whether a backfill can present `id`'s full history: it has not stepped
    /// yet, it keeps its integral, or it can be recomputed from nodes that can.
    fn replayable(&self, id: NodeId) -> bool {
        id.0 >= self.stepped
            || self.keep[id.0]
            || (self.nodes[id.0].is_replayable() && self.nodes[id.0].children().iter().all(|c| self.replayable(*c)))
    }

    /// Demand `id`'s integral. A node that stepped without one can start
    /// keeping one only if it is replayable, and then its integral is rebuilt
    /// at the next backfill; node sharing only ever hands out such nodes.
    ///
    /// This is where the integral's representation is picked (P-4b): a
    /// column when the node's keys are one sort's ids, else general.
    fn keep_integral(&mut self, id: NodeId) {
        if !self.keep[id.0] {
            if id.0 < self.stepped {
                assert!(self.replayable(id), "node {} needs an integral it never kept", id.0);
                self.revive.push(id);
            }
            self.keep[id.0] = true;
            self.integrals[id.0] = Integral::empty(column_sort(self, id));
        }
    }

    /// Get or create the input node for a base table. Called both by lowering
    /// (a view mentions the field) and by data ingestion (a `new` writes it),
    /// in either order.
    pub fn input(&mut self, key: InputKey) -> NodeId {
        if let Some(id) = self.inputs.get(&key) {
            return *id;
        }
        let id = self.add_node(Node::Input(key));
        self.inputs.insert(key, id);
        id
    }

    /// Register a lowered recursion group's nested circuit; returns the region
    /// index the group's `FixOutput` nodes name.
    pub(crate) fn add_fix_region(
        &mut self,
        inner: Circuit,
        imports: Vec<NodeId>,
        import_nodes: Vec<NodeId>,
        rec_inputs: Vec<NodeId>,
        member_outs: Vec<NodeId>,
    ) -> usize {
        let members = member_outs.len();
        let mut inner = inner;
        for &m in &member_outs {
            inner.keep_integral(m); // the knot reads each member's integral
        }
        self.fixes.push(FixRegion {
            inner,
            imports,
            import_nodes,
            rec_inputs,
            member_outs,
            prev: vec![Batch::new(); members],
            fired: false,
        });
        self.fixes.len() - 1
    }

    /// Clear every integral and every node's private state — a fix region's
    /// inner circuit is re-derived from scratch each outer step.
    fn reset_state(&mut self) {
        for node in &mut self.nodes {
            node.reset();
        }
        for i in 0..self.nodes.len() {
            let column = if self.keep[i] { column_sort(self, NodeId(i)) } else { None };
            self.integrals[i] = Integral::empty(column);
        }
    }

    /// One inner-clock pass for a fix region: compute every node in arena
    /// order from the seeded deltas (`FixInput`s carry the Enter/feedback
    /// signal), then commit, returning the pass's deltas so the knot can
    /// clamp incrementally. Same discipline as [`Circuit::run`], minus
    /// transaction seeding and view reporting. Integrals persist across
    /// iterations — that persistence is the semi-naive optimization.
    fn iterate(&mut self, mut deltas: Vec<Batch>) -> Vec<Batch> {
        for i in 0..self.nodes.len() {
            if matches!(self.nodes[i], Node::Input(_) | Node::FixInput) {
                continue; // seeded, never computed
            }
            let (prev, rest) = deltas.split_at_mut(i);
            let ctx = Ctx { deltas: prev, integrals: &self.integrals, keep: &self.keep, floor: 0 };
            let quiescent = !matches!(self.nodes[i], Node::ConstSingleton { fired: false, .. })
                && self.nodes[i].children().iter().all(|c| ctx.delta(*c).is_empty());
            if quiescent {
                continue;
            }
            rest[0] = self.nodes[i].compute(&ctx);
        }
        self.commit(&deltas, 0, &[]);
        deltas
    }

    /// Fold `deltas[from..]`, and the replayed deltas below `from`, into the
    /// integrals of the nodes that keep one.
    fn commit(&mut self, deltas: &[Batch], from: usize, replay: &[bool]) {
        for (i, delta) in deltas.iter().enumerate() {
            if self.keep[i] && (i >= from || replay.get(i) == Some(&true)) {
                self.integrals[i].commit(delta);
            }
        }
        self.stepped = self.nodes.len();
    }

    /// Register `node` as the producer of the named view. Any node may be
    /// named, including an input or another view's node: lowering's rewrites
    /// (P-1) can reduce a view to a node that already exists.
    pub fn set_output(&mut self, name: &str, node: NodeId) {
        self.keep_integral(node); // read back by `view` (snapshots, keyset targets)
        self.outputs.insert(name.to_string(), node);
    }

    pub fn output(&self, name: &str) -> Option<NodeId> {
        self.outputs.get(name).copied()
    }

    /// The integrated (full) contents of a named view.
    pub fn view(&self, name: &str) -> Option<&Integral> {
        self.outputs.get(name).map(|id| &self.integrals[id.0])
    }

    /// Every registered view name (for a full snapshot at boot).
    pub fn output_names(&self) -> impl Iterator<Item = &String> {
        self.outputs.keys()
    }

    /// A node of the arena (lowering inspects already-built nodes to decide
    /// its rewrites).
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0]
    }

    /// The integrated output of a node that keeps one (an input, a view, or a
    /// node some kernel reads whole — see [`Node::integral_reads`]).
    pub fn integral(&self, id: NodeId) -> &Integral {
        assert!(self.keep[id.0], "node {} keeps no integral", id.0);
        &self.integrals[id.0]
    }

    /// The integrated contents of a base table, if it has been touched.
    pub fn input_integral(&self, key: &InputKey) -> Option<&Integral> {
        self.inputs.get(key).map(|id| &self.integrals[id.0])
    }

    /// Whether `id` maintains an integral (P-2's demand analysis).
    pub fn keeps_integral(&self, id: NodeId) -> bool {
        self.keep[id.0]
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
        let deltas = self.seed_deltas(tx);
        self.run(0, &[], deltas, true)
    }

    /// Run one transaction purely for its effect on state, skipping the
    /// per-view delta collection `step` does (S-21) — the engine-only-cost
    /// path replay wants: a page reload re-applies a whole log with nobody
    /// listening for per-step deltas.
    pub fn step_silent(&mut self, tx: &Transaction) {
        let deltas = self.seed_deltas(tx);
        self.run(0, &[], deltas, false);
    }

    /// Ensure every targeted base table exists (so data arriving ahead of
    /// any view that reads it is kept), then seed each input node's delta
    /// from the transaction.
    fn seed_deltas(&mut self, tx: &Transaction) -> Vec<Batch> {
        for (key, _, _, _) in &tx.deltas {
            self.input(*key);
        }
        let mut rows: Vec<Vec<Row>> = vec![Vec::new(); self.nodes.len()];
        for (key, l, r, w) in &tx.deltas {
            rows[self.inputs[key].0].push((l.clone(), r.clone(), *w));
        }
        rows.into_iter().map(Batch::from_rows).collect()
    }

    /// Evaluate the freshly appended node suffix `from..` over the data already
    /// in the circuit (a new `let` over existing base tables), and report the
    /// full contents of `views` — the lets being added — as their first delta.
    /// Every node below `from` presents its full integral as one first delta —
    /// and reads as an *empty* integral, so the history is seen exactly once.
    /// Because every delta rule is exact, δ-from-empty equals batch evaluation
    /// by construction. Pre-existing nodes are neither recomputed nor
    /// recommitted; the ones a new node reads have their integral copied into
    /// their delta slot as a batch.
    ///
    /// `views` is named explicitly because a view may alias a node below
    /// `from` (P-1: `.num` at a row level *is* the `num` input), which
    /// the suffix alone would not reveal.
    ///
    /// A below-floor node that never kept an integral, or only just started
    /// to (`revive`), is instead reset and recomputed in this pass from its
    /// children's history (`Node::is_replayable`), which yields its full
    /// value and rebuilds its private state as it was. A revived node commits
    /// that value as its first integral.
    pub fn backfill(&mut self, from: usize, views: &[&str]) -> StepResult {
        // Below-floor nodes a new node reads, transitively through the ones
        // that cannot present their history as an integral.
        let revived = std::mem::take(&mut self.revive);
        let mut replay = vec![false; self.nodes.len()];
        let mut deltas = vec![Batch::new(); self.nodes.len()];
        let mut stack: Vec<NodeId> = revived.clone();
        for node in &self.nodes[from..] {
            stack.extend(node.children());
        }
        while let Some(id) = stack.pop() {
            if id.0 >= from || replay[id.0] {
                continue;
            }
            if self.keep[id.0] && !revived.contains(&id) {
                // Presents its history as its delta (idempotent on revisit).
                deltas[id.0] = self.integrals[id.0].to_batch();
                continue;
            }
            assert!(self.nodes[id.0].is_replayable(), "backfill cannot recompute node {}", id.0);
            replay[id.0] = true;
            stack.extend(self.nodes[id.0].children());
        }
        self.run(from, &replay, deltas, false);
        let mut result = StepResult::default();
        for name in views {
            let id = self.outputs[*name];
            result.view_deltas.insert(name.to_string(), self.integrals[id.0].to_batch());
        }
        result
    }

    /// The shared driver: compute the below-floor nodes marked in `replay`
    /// (a backfill's recomputed ones), then nodes `floor..`, in topological
    /// order; commit their deltas, and — when `collect` — report every view's
    /// delta.
    /// `collect: false` (S-21's silent replay path)
    /// skips only that final per-view clone; every node still computes and
    /// commits, so state after a silent step is identical to a normal one.
    fn run(&mut self, floor: usize, replay: &[bool], mut deltas: Vec<Batch>, collect: bool) -> StepResult {
        assert!(self.revive.is_empty(), "a node started keeping an integral; backfill before stepping");
        // Per-step cache of each fix region's member deltas (a region is
        // evaluated once even though it has one FixOutput per member).
        let mut region_cache: Vec<Option<Vec<Batch>>> = vec![None; self.fixes.len()];

        // Phase 1: compute. Shared integrals stay pre-step throughout; nodes
        // below `floor` keep their seeded deltas and read as empty integrals.
        let replayed = (0..floor).filter(|&i| replay[i]);
        for i in replayed.chain(floor..self.nodes.len()) {
            if matches!(self.nodes[i], Node::Input(_)) {
                continue; // seeded, never computed
            }
            if i < floor {
                // Replayed: start from a fresh node (an unfired singleton, an
                // unseeded aggregate) and rebuild its state from the history.
                self.nodes[i].reset();
            }
            let (prev, rest) = deltas.split_at_mut(i);
            let ctx = Ctx { deltas: prev, integrals: &self.integrals, keep: &self.keep, floor };
            // Dirty-cone skip: a node whose children all sit still this step
            // emits nothing and changes no state. Three fire-from-no-children
            // exceptions: an unfired ConstSingleton; a FixOutput whose region
            // has never evaluated (an import-less, constant-only group has no
            // child deltas to ever wake it); and an unseeded *total* aggregate
            // (S-50), whose group key exists by construction and so must emit
            // its identity — `count(Todo by unit)` is `0` with no todos, and
            // with no todos there is no delta to wake it with.
            let quiescent = match &self.nodes[i] {
                Node::ConstSingleton { fired: false, .. } => false,
                Node::Aggregate { total: Total::Unit, seeded: false, .. } => false,
                Node::FixOutput { region, .. } if !self.fixes[*region].fired => false,
                node => node.children().iter().all(|c| ctx.delta(*c).is_empty()),
            };
            if quiescent {
                continue;
            }
            if let Node::FixOutput { region, member, .. } = &self.nodes[i] {
                // Driven by the region, not computed: evaluate the nested
                // fixpoint once per step and hand each member its delta.
                let (region, member) = (*region, *member);
                let cached = region_cache[region]
                    .get_or_insert_with(|| self.fixes[region].evaluate(&ctx));
                rest[0] = cached[member].clone();
                continue;
            }
            rest[0] = self.nodes[i].compute(&ctx);
        }

        // Phase 2: commit — fold computed deltas into the integrals kept.
        self.commit(&deltas, floor, replay);

        // Hand each view its node's delta, moved out; only a node that several
        // views alias (P-1) is cloned.
        let mut result = StepResult::default();
        if collect {
            let mut first: HashMap<NodeId, &String> = HashMap::new();
            for (name, id) in &self.outputs {
                let delta = match first.get(id) {
                    Some(other) => result.view_deltas[*other].clone(),
                    None => {
                        first.insert(*id, name);
                        std::mem::take(&mut deltas[id.0])
                    }
                };
                result.view_deltas.insert(name.clone(), delta);
            }
        }
        result
    }
}
