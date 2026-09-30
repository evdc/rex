//! Lowering: the elaborated [`TExpr`] tree to circuit nodes (§10 — the
//! point-free program *is* the circuit).
//!
//! A straight recursive walk: children are lowered before parents, preserving
//! the arena's topological invariant. Base tables dedup through
//! [`Circuit::input`]; `View` references resolve to the producing node of an
//! already-lowered `let` (so `add_view` must be called in program order).
//! Money/Int tagging and the `By`/antijoin desugarings mirror the batch
//! interpreter (`eval::interp`) exactly — it is the semantics oracle.
//!
//! A recursion group (§8) lowers through [`lower_group`]: its bodies go into a
//! fresh *inner* circuit, with every reference to the outer world (base
//! tables, earlier views) routed through a deduped `FixInput` import and every
//! member reference (`RecVar`) routed to that member's feedback slot. The
//! outer circuit gets one `FixOutput` per member.

use super::circuit::Circuit;
use super::node::{CoKeyedFn, InputKey, Node, NodeId, Scalar};
use crate::eval::intern::intern;
use crate::eval::interp::lit_value;
use crate::eval::relation::BTreeRelation;
use crate::eval::value::Value;
use crate::types::ty::{SortId, ValueTy};
use crate::types::typed::{TExpr, TExprKind};
use std::collections::{BTreeSet, HashMap};

/// Lower `te` into `circuit`, returning the node that produces its relation.
/// `values` resolves `new`-bound entity ids (a `ValueRef`'s `new` always
/// precedes the `let` that mentions it, so the id is known at lowering time).
pub fn lower(circuit: &mut Circuit, te: &TExpr, values: &HashMap<String, Value>) -> NodeId {
    lower_in(circuit, &mut None, te, values)
}

/// Lower one recursion group: build the inner circuit, register the fix
/// region, and return the outer `FixOutput` node for each member (in binding
/// order).
pub fn lower_group(
    circuit: &mut Circuit,
    bindings: &[(String, TExpr)],
    values: &HashMap<String, Value>,
) -> Vec<NodeId> {
    let mut inner = Circuit::new();
    let mut scope = FixScope {
        outer: circuit,
        imports: Vec::new(),
        import_nodes: Vec::new(),
        rec: HashMap::new(),
    };
    // Feedback slots first: every body must be able to name every member.
    let mut rec_inputs = Vec::new();
    for (name, _) in bindings {
        let slot = inner.add_node(Node::FixInput);
        scope.rec.insert(name.clone(), slot);
        rec_inputs.push(slot);
    }
    let mut scope = Some(scope);
    let member_outs: Vec<NodeId> = bindings
        .iter()
        .map(|(_, body)| lower_in(&mut inner, &mut scope, body, values))
        .collect();
    let FixScope { imports, import_nodes, .. } = scope.take().expect("scope persists");

    let region = circuit.add_fix_region(inner, imports.clone(), import_nodes, rec_inputs, member_outs);
    (0..bindings.len())
        .map(|member| {
            circuit.add_node(Node::FixOutput { region, member, imports: imports.clone() })
        })
        .collect()
}

/// Lowering context inside a recursion group: `circuit` in [`lower_in`] is the
/// *inner* circuit, and everything external resolves against `outer` and
/// enters through a deduped import.
struct FixScope<'a> {
    outer: &'a mut Circuit,
    /// Outer nodes feeding the region, parallel to `import_nodes`.
    imports: Vec<NodeId>,
    /// The inner `FixInput` for each import.
    import_nodes: Vec<NodeId>,
    /// Group member name -> inner feedback slot.
    rec: HashMap<String, NodeId>,
}

impl FixScope<'_> {
    /// The inner face of an outer node, deduped per outer id.
    fn import(&mut self, inner: &mut Circuit, outer: NodeId) -> NodeId {
        if let Some(pos) = self.imports.iter().position(|&o| o == outer) {
            return self.import_nodes[pos];
        }
        let node = inner.add_node(Node::FixInput);
        self.imports.push(outer);
        self.import_nodes.push(node);
        node
    }
}

/// A base-table input: direct, or imported into the inner circuit in a scope.
fn input_node(circuit: &mut Circuit, scope: &mut Option<FixScope>, key: InputKey) -> NodeId {
    match scope {
        Some(s) => {
            let outer = s.outer.input(key);
            s.import(circuit, outer)
        }
        None => circuit.input(key),
    }
}

fn lower_in(
    circuit: &mut Circuit,
    scope: &mut Option<FixScope>,
    te: &TExpr,
    values: &HashMap<String, Value>,
) -> NodeId {
    let low = lower_low(circuit, scope, te, values);
    materialize(circuit, scope, low)
}

/// A lowered subexpression not yet committed to a node, so that its parent
/// can still fuse it (P-1). Building the node eagerly and then fusing would
/// leave the unfused node in the arena, computing every step for nobody.
enum Low {
    Node(NodeId),
    /// `Const { lit, dom }`: `(id, value)` at weight 1 for every live `dom` id.
    Const { dom: SortId, value: Value },
    /// `f` applied to each row of `input`, whose left keys are live `sort` ids.
    Map { input: NodeId, sort: SortId, f: Scalar },
}

fn materialize(circuit: &mut Circuit, scope: &mut Option<FixScope>, low: Low) -> NodeId {
    match low {
        Low::Node(id) | Low::Map { input: id, f: Scalar::Val, .. } => id,
        Low::Const { dom, value } => {
            let ids = input_node(circuit, scope, InputKey::Identity(dom));
            circuit.add_node(Node::MapConst(ids, value))
        }
        Low::Map { input, f, .. } => circuit.add_node(Node::FilterMap(input, f)),
    }
}

// --- P-1 rewrites -------------------------------------------------------------
//
// Lowering deletes circuit that the engine's *base invariant* makes redundant
// (asserted in `Engine::build_tx`, property-tested in `tests/lower_rewrites.rs`):
//
// - an identity input `Identity(E)` holds `(id, id)` at weight 1 exactly for
//   the live ids of `E`;
// - a field input `Field(E, f)` holds at most one row per id, at weight 1, and
//   only for live ids ("no orphans").
//
// From it: `Identity(E) . X` is `X` when X's left keys are live `E` ids; a
// co-keyed op against a constant over the same live ids is a per-row map; and
// `Identity(E)[X]` is `X` when X is already a weight-1 coreflexive on live ids.
// The rewrites inspect *nodes*, so a fix region's inner circuit (whose inputs
// are `FixInput` imports) is never rewritten.

/// The sort whose live ids bound `id`'s left keys at every step boundary, if
/// the node's construction guarantees one.
fn anchor(c: &Circuit, id: NodeId) -> Option<SortId> {
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

/// Whether `id` holds at most one row per left key, at weight 1.
fn functional(c: &Circuit, id: NodeId) -> bool {
    match c.node(id) {
        Node::Input(_) | Node::Aggregate { .. } => true,
        Node::MapConst(a, _) | Node::FilterMap(a, _) | Node::Filter(a, _) | Node::InRel(a, _) => functional(c, *a),
        _ => false,
    }
}

fn low_anchor(c: &Circuit, low: &Low) -> Option<SortId> {
    match low {
        Low::Node(id) => anchor(c, *id),
        Low::Const { dom, .. } => Some(*dom),
        Low::Map { sort, .. } => Some(*sort),
    }
}

/// Whether `low` is a weight-1 coreflexive whose keys are live `sort` ids —
/// exactly what `Identity(sort)[low]` would produce.
fn coreflexive_on(c: &Circuit, low: &Low, sort: SortId) -> bool {
    match low {
        Low::Map { input, sort: s, f } => *s == sort && f.is_coreflexive() && functional(c, *input),
        Low::Node(id) => {
            anchor(c, *id) == Some(sort)
                && match c.node(*id) {
                    Node::Input(InputKey::Identity(_)) => true,
                    Node::FilterMap(a, f) => f.is_coreflexive() && functional(c, *a),
                    Node::InRel(a, _) => functional(c, *a),
                    _ => false,
                }
        }
        Low::Const { .. } => false,
    }
}

/// `low` as a per-row map over one anchored input, if it is one.
fn as_map(c: &Circuit, low: &Low) -> Option<(NodeId, SortId, Scalar)> {
    match low {
        Low::Map { input, sort, f } => Some((*input, *sort, f.clone())),
        Low::Node(id) => anchor(c, *id).map(|s| (*id, s, Scalar::Val)),
        Low::Const { .. } => None,
    }
}

/// Fuse `a f b` into one per-row map when one side is a constant over the
/// live ids the other side is anchored to.
fn fuse(c: &Circuit, a: &Low, b: &Low, f: &CoKeyedFn) -> Option<Low> {
    let (map, value, map_left) = match (a, b) {
        (x, Low::Const { dom, value }) | (Low::Const { dom, value }, x @ (Low::Node(_) | Low::Map { .. }))
            if low_anchor(c, x) == Some(*dom) =>
        {
            (x, value, std::ptr::eq(x, a))
        }
        _ => return None,
    };
    let (input, sort, e) = as_map(c, map)?;
    let k = Box::new(Scalar::Const(value.clone()));
    let (l, r) = if map_left { (Box::new(e), k) } else { (k, Box::new(e)) };
    Some(Low::Map { input, sort, f: Scalar::Bin(f.clone(), l, r) })
}

fn lower_low(
    circuit: &mut Circuit,
    scope: &mut Option<FixScope>,
    te: &TExpr,
    values: &HashMap<String, Value>,
) -> Low {
    let node = match &te.kind {
        TExprKind::Const { lit, dom } => return Low::Const { dom: *dom, value: lit_value(lit) },
        TExprKind::Compose(a, b) => {
            let l = lower_in(circuit, scope, a, values);
            let r = lower_low(circuit, scope, b, values);
            if let Node::Input(InputKey::Identity(s)) = circuit.node(l)
                && low_anchor(circuit, &r) == Some(*s)
            {
                return r; // Identity(E) . X = X: X's keys are already live E ids.
            }
            let r = materialize(circuit, scope, r);
            Node::Compose { l, r, linv: BTreeRelation::new() }
        }
        TExprKind::Semijoin(a, b) => {
            let l = lower_in(circuit, scope, a, values);
            let r = lower_low(circuit, scope, b, values);
            if let Node::Input(InputKey::Identity(s)) = circuit.node(l)
                && coreflexive_on(circuit, &r, *s)
            {
                return r; // Identity(E)[X] = X for a weight-1 coreflexive X on E.
            }
            let r = materialize(circuit, scope, r);
            Node::Semijoin { l, r, linv: BTreeRelation::new() }
        }
        TExprKind::Fork(a, b) => return co_keyed(circuit, scope, a, b, CoKeyedFn::Fork, values),
        TExprKind::Mul(a, b) => {
            let money = te.ty.to == ValueTy::Money;
            return co_keyed(circuit, scope, a, b, CoKeyedFn::Mul { money }, values);
        }
        TExprKind::Concat(a, b) => return co_keyed(circuit, scope, a, b, CoKeyedFn::Concat, values),
        TExprKind::Arith(kind, a, b) => {
            let money = te.ty.to == ValueTy::Money;
            return co_keyed(circuit, scope, a, b, CoKeyedFn::Arith { kind: *kind, money }, values);
        }
        TExprKind::BinCompare(op, a, b) => {
            return co_keyed(circuit, scope, a, b, CoKeyedFn::Compare(*op), values);
        }
        _ => return Low::Node(lower_plain(circuit, scope, te, values)),
    };
    Low::Node(circuit.add_node(node))
}

/// Every expression form that lowering never rewrites.
fn lower_plain(
    circuit: &mut Circuit,
    scope: &mut Option<FixScope>,
    te: &TExpr,
    values: &HashMap<String, Value>,
) -> NodeId {
    match &te.kind {
        TExprKind::Identity(sort) => input_node(circuit, scope, InputKey::Identity(*sort)),
        TExprKind::View(name) => match scope {
            Some(s) => {
                let outer = s
                    .outer
                    .output(name)
                    .unwrap_or_else(|| panic!("view `{name}` referenced before it was lowered"));
                s.import(circuit, outer)
            }
            None => circuit
                .output(name)
                .unwrap_or_else(|| panic!("view `{name}` referenced before it was lowered")),
        },
        TExprKind::RecVar(name) => {
            let s = scope.as_ref().expect("RecVar occurs only inside a recursion group");
            *s.rec.get(name).unwrap_or_else(|| panic!("unbound RecVar `{name}`"))
        }
        TExprKind::ValueRef(name) => {
            let v = values
                .get(name)
                .cloned()
                .unwrap_or_else(|| panic!("unbound value `{name}` at lowering"));
            circuit.add_node(Node::ConstSingleton { value: v, fired: false })
        }
        TExprKind::Field(hops) => {
            let mut acc = input_node(circuit, scope, InputKey::Field(hops[0].sort, intern(&hops[0].field)));
            for hop in &hops[1..] {
                let next = input_node(circuit, scope, InputKey::Field(hop.sort, intern(&hop.field)));
                acc = circuit.add_node(Node::Compose {
                    l: acc,
                    r: next,
                    linv: BTreeRelation::new(),
                });
            }
            acc
        }
        TExprKind::UnitConst(lit) => {
            let point = circuit.add_node(Node::ConstSingleton { value: Value::Unit, fired: false });
            circuit.add_node(Node::MapConst(point, lit_value(lit)))
        }
        TExprKind::UnitPoint => circuit.add_node(Node::ConstSingleton {
            value: Value::Unit,
            fired: false,
        }),
        TExprKind::Atom(a) => circuit.add_node(Node::ConstSingleton {
            value: Value::atom(a),
            fired: false,
        }),
        TExprKind::Filter(a, pred) => {
            let l = lower_in(circuit, scope, a, values);
            circuit.add_node(Node::Filter(l, pred.clone()))
        }
        TExprKind::Union(a, b) => {
            let l = lower_in(circuit, scope, a, values);
            let r = lower_in(circuit, scope, b, values);
            circuit.add_node(Node::Union(l, r))
        }
        TExprKind::Intersect(a, b) => {
            let l = lower_in(circuit, scope, a, values);
            let r = lower_in(circuit, scope, b, values);
            circuit.add_node(Node::Intersect(l, r))
        }
        TExprKind::Inverse(a) => {
            let l = lower_in(circuit, scope, a, values);
            circuit.add_node(Node::Inverse(l))
        }
        TExprKind::Distinct(a) => {
            let l = lower_in(circuit, scope, a, values);
            circuit.add_node(Node::Distinct(l))
        }
        TExprKind::Proj(side, a) => {
            let l = lower_in(circuit, scope, a, values);
            circuit.add_node(Node::Proj(l, *side))
        }
        TExprKind::By(x, y) => {
            // X by Y == ~Y . X, exactly as the interpreter desugars it.
            let yi = lower_in(circuit, scope, y, values);
            let inv = circuit.add_node(Node::Inverse(yi));
            let xi = lower_in(circuit, scope, x, values);
            circuit.add_node(Node::Compose { l: inv, r: xi, linv: BTreeRelation::new() })
        }
        TExprKind::Antijoin(a, b) => {
            let l = lower_in(circuit, scope, a, values);
            let r = lower_in(circuit, scope, b, values);
            circuit.add_node(Node::Antijoin { l, r, linv: BTreeRelation::new() })
        }
        TExprKind::Coreflexive(pred) => {
            // Materializable only over an enumerable (entity) domain; the
            // groundedness pass guarantees this on a checked program (same
            // contract as the batch interpreter's Coreflexive arm).
            let ValueTy::Id(sort) = te.ty.from else {
                panic!("groundedness pass guarantees an enumerable domain")
            };
            let ids = input_node(circuit, scope, InputKey::Identity(sort));
            circuit.add_node(Node::Filter(ids, pred.clone()))
        }
        TExprKind::InRel(a, lits) => {
            let l = lower_in(circuit, scope, a, values);
            let set: BTreeSet<Value> = lits.iter().map(lit_value).collect();
            circuit.add_node(Node::InRel(l, set))
        }
        TExprKind::Agg(kind, arg, total) => {
            let money = arg.ty.to == ValueTy::Money;
            let input = lower_in(circuit, scope, arg, values);
            circuit.add_node(Node::Aggregate {
                input,
                kind: *kind,
                money,
                total: *total,
                seeded: false,
                st: Default::default(),
            })
        }
        TExprKind::Const { .. }
        | TExprKind::Compose(..)
        | TExprKind::Semijoin(..)
        | TExprKind::Fork(..)
        | TExprKind::Mul(..)
        | TExprKind::Concat(..)
        | TExprKind::Arith(..)
        | TExprKind::BinCompare(..) => unreachable!("lowered by `lower_low`"),
    }
}

fn co_keyed(
    circuit: &mut Circuit,
    scope: &mut Option<FixScope>,
    a: &TExpr,
    b: &TExpr,
    f: CoKeyedFn,
    values: &HashMap<String, Value>,
) -> Low {
    let la = lower_low(circuit, scope, a, values);
    let lb = lower_low(circuit, scope, b, values);
    if let Some(fused) = fuse(circuit, &la, &lb, &f) {
        return fused;
    }
    let l = materialize(circuit, scope, la);
    let r = materialize(circuit, scope, lb);
    Low::Node(circuit.add_node(Node::CoKeyed { l, r, f }))
}
