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
use super::node::{CoKeyedFn, InputKey, Node, NodeId};
use crate::eval::intern::intern;
use crate::eval::interp::lit_value;
use crate::eval::relation::BTreeRelation;
use crate::eval::value::Value;
use crate::types::ty::ValueTy;
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
        TExprKind::Const { lit, dom } => {
            let ids = input_node(circuit, scope, InputKey::Identity(*dom));
            circuit.add_node(Node::MapConst(ids, lit_value(lit)))
        }
        TExprKind::Atom(a) => circuit.add_node(Node::ConstSingleton {
            value: Value::atom(a),
            fired: false,
        }),

        TExprKind::Compose(a, b) => {
            let l = lower_in(circuit, scope, a, values);
            let r = lower_in(circuit, scope, b, values);
            circuit.add_node(Node::Compose { l, r, linv: BTreeRelation::new() })
        }
        TExprKind::Semijoin(a, b) => {
            let l = lower_in(circuit, scope, a, values);
            let r = lower_in(circuit, scope, b, values);
            circuit.add_node(Node::Semijoin { l, r, linv: BTreeRelation::new() })
        }
        TExprKind::Filter(a, pred) => {
            let l = lower_in(circuit, scope, a, values);
            circuit.add_node(Node::Filter(l, pred.clone()))
        }
        TExprKind::Fork(a, b) => co_keyed(circuit, scope, a, b, CoKeyedFn::Fork, values),
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
        TExprKind::Mul(a, b) => {
            let money = te.ty.to == ValueTy::Money;
            co_keyed(circuit, scope, a, b, CoKeyedFn::Mul { money }, values)
        }
        TExprKind::Concat(a, b) => co_keyed(circuit, scope, a, b, CoKeyedFn::Concat, values),

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
        TExprKind::BinCompare(op, a, b) => {
            co_keyed(circuit, scope, a, b, CoKeyedFn::Compare(*op), values)
        }
        TExprKind::InRel(a, lits) => {
            let l = lower_in(circuit, scope, a, values);
            let set: BTreeSet<Value> = lits.iter().map(lit_value).collect();
            circuit.add_node(Node::InRel(l, set))
        }
        TExprKind::Agg(kind, arg) => {
            let money = arg.ty.to == ValueTy::Money;
            let input = lower_in(circuit, scope, arg, values);
            circuit.add_node(Node::Aggregate {
                input,
                kind: *kind,
                money,
                st: Default::default(),
            })
        }
    }
}

fn co_keyed(
    circuit: &mut Circuit,
    scope: &mut Option<FixScope>,
    a: &TExpr,
    b: &TExpr,
    f: CoKeyedFn,
    values: &HashMap<String, Value>,
) -> NodeId {
    let l = lower_in(circuit, scope, a, values);
    let r = lower_in(circuit, scope, b, values);
    circuit.add_node(Node::CoKeyed { l, r, f })
}
