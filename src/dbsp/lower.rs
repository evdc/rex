//! Lowering: the elaborated [`TExpr`] tree to circuit nodes (§10 — the
//! point-free program *is* the circuit).
//!
//! A straight recursive walk: children are lowered before parents, preserving
//! the arena's topological invariant. Base tables dedup through
//! [`Circuit::input`]; `View` references resolve to the producing node of an
//! already-lowered `let` (so `add_view` must be called in program order).
//! Money/Int tagging and the `By`/antijoin desugarings mirror the batch
//! interpreter (`eval::interp`) exactly — it is the semantics oracle.

use super::circuit::Circuit;
use super::node::{CoKeyedFn, InputKey, Node, NodeId};
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
    match &te.kind {
        TExprKind::Identity(sort) => circuit.input(InputKey::Identity(*sort)),
        TExprKind::View(name) => circuit
            .output(name)
            .unwrap_or_else(|| panic!("view `{name}` referenced before it was lowered")),
        TExprKind::ValueRef(name) => {
            let v = values
                .get(name)
                .cloned()
                .unwrap_or_else(|| panic!("unbound value `{name}` at lowering"));
            circuit.add_node(Node::ConstSingleton { value: v, fired: false })
        }
        TExprKind::Field(hops) => {
            let mut acc = circuit.input(InputKey::Field(hops[0].sort, hops[0].field.clone()));
            for hop in &hops[1..] {
                let next = circuit.input(InputKey::Field(hop.sort, hop.field.clone()));
                acc = circuit.add_node(Node::Compose {
                    l: acc,
                    r: next,
                    linv: BTreeRelation::new(),
                });
            }
            acc
        }
        TExprKind::Const { lit, dom } => {
            let ids = circuit.input(InputKey::Identity(*dom));
            circuit.add_node(Node::MapConst(ids, lit_value(lit)))
        }
        TExprKind::Atom(a) => circuit.add_node(Node::ConstSingleton {
            value: Value::Atom(a.clone()),
            fired: false,
        }),

        TExprKind::Compose(a, b) => {
            let l = lower(circuit, a, values);
            let r = lower(circuit, b, values);
            circuit.add_node(Node::Compose { l, r, linv: BTreeRelation::new() })
        }
        TExprKind::Semijoin(a, b) => {
            let l = lower(circuit, a, values);
            let r = lower(circuit, b, values);
            circuit.add_node(Node::Semijoin { l, r, linv: BTreeRelation::new() })
        }
        TExprKind::Filter(a, pred) => {
            let l = lower(circuit, a, values);
            circuit.add_node(Node::Filter(l, pred.clone()))
        }
        TExprKind::Fork(a, b) => co_keyed(circuit, a, b, CoKeyedFn::Fork, values),
        TExprKind::Union(a, b) => {
            let l = lower(circuit, a, values);
            let r = lower(circuit, b, values);
            circuit.add_node(Node::Union(l, r))
        }
        TExprKind::Intersect(a, b) => {
            let l = lower(circuit, a, values);
            let r = lower(circuit, b, values);
            circuit.add_node(Node::Intersect(l, r))
        }
        TExprKind::Inverse(a) => {
            let l = lower(circuit, a, values);
            circuit.add_node(Node::Inverse(l))
        }
        TExprKind::Distinct(a) => {
            let l = lower(circuit, a, values);
            circuit.add_node(Node::Distinct(l))
        }
        TExprKind::By(x, y) => {
            // X by Y == ~Y . X, exactly as the interpreter desugars it.
            let yi = lower(circuit, y, values);
            let inv = circuit.add_node(Node::Inverse(yi));
            let xi = lower(circuit, x, values);
            circuit.add_node(Node::Compose { l: inv, r: xi, linv: BTreeRelation::new() })
        }
        TExprKind::Antijoin(a, b) => {
            let l = lower(circuit, a, values);
            let r = lower(circuit, b, values);
            circuit.add_node(Node::Antijoin { l, r, linv: BTreeRelation::new() })
        }
        TExprKind::Mul(a, b) => {
            let money = te.ty.to == ValueTy::Money;
            co_keyed(circuit, a, b, CoKeyedFn::Mul { money }, values)
        }
        TExprKind::Concat(a, b) => co_keyed(circuit, a, b, CoKeyedFn::Concat, values),

        TExprKind::Coreflexive(pred) => {
            // Materializable only over an enumerable (entity) domain; the
            // groundedness pass guarantees this on a checked program (same
            // contract as the batch interpreter's Coreflexive arm).
            let ValueTy::Id(sort) = te.ty.from else {
                panic!("groundedness pass guarantees an enumerable domain")
            };
            let ids = circuit.input(InputKey::Identity(sort));
            circuit.add_node(Node::Filter(ids, pred.clone()))
        }
        TExprKind::BinCompare(op, a, b) => {
            co_keyed(circuit, a, b, CoKeyedFn::Compare(*op), values)
        }
        TExprKind::InRel(a, lits) => {
            let l = lower(circuit, a, values);
            let set: BTreeSet<Value> = lits.iter().map(lit_value).collect();
            circuit.add_node(Node::InRel(l, set))
        }
        TExprKind::Agg(kind, arg) => {
            let money = arg.ty.to == ValueTy::Money;
            let input = lower(circuit, arg, values);
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
    a: &TExpr,
    b: &TExpr,
    f: CoKeyedFn,
    values: &HashMap<String, Value>,
) -> NodeId {
    let l = lower(circuit, a, values);
    let r = lower(circuit, b, values);
    circuit.add_node(Node::CoKeyed { l, r, f })
}
