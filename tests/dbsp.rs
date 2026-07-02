//! Property tests for the incremental backend, oracled by the batch algebra.
//!
//! Principle: `eval::algebra` defines the semantics. For each operator we build
//! a one-node circuit, feed it a random sequence of transactions (tiny domains
//! and mixed-sign weights, to force key collisions, cancellations to zero, and
//! retractions), and after **every** step assert that the node's integrated
//! output equals the batch kernel applied to the integrated inputs.

use proptest::prelude::*;
use rex::ast::CmpOp;
use rex::dbsp::{Circuit, CoKeyedFn, InputKey, Node, NodeId, Transaction};
use rex::eval::relation::{BTreeRelation, BinaryRelation};
use rex::eval::{Value, algebra};
use rex::types::ty::SortId;
use rex::types::typed::{Lit, Pred};
use std::collections::BTreeSet;

fn int(n: i64) -> Value {
    Value::Int(n)
}

fn key_a() -> InputKey {
    InputKey::Field(SortId(0), "a".to_string())
}

fn key_b() -> InputKey {
    InputKey::Field(SortId(0), "b".to_string())
}

/// One base-table change: (left, right, weight) over a tiny domain.
type Change = (i64, i64, i64);

/// A transaction sequence: outer = steps, inner = changes within one step.
fn tx_seq() -> impl Strategy<Value = Vec<Vec<Change>>> {
    prop::collection::vec(
        prop::collection::vec((0i64..5, 0i64..5, -2i64..=2), 0..6),
        1..12,
    )
}

fn to_tx(key: &InputKey, changes: &[Change]) -> Transaction {
    let mut tx = Transaction::new();
    for &(l, r, w) in changes {
        tx.push(key.clone(), int(l), int(r), w);
    }
    tx
}

fn integrate(oracle: &mut BTreeRelation, changes: &[Change]) {
    for &(l, r, w) in changes {
        oracle.add(int(l), int(r), w);
    }
}

/// Drive a one-input operator: after every step, the node's integral must equal
/// `batch` applied to the integrated input.
fn check_unary(
    node: impl Fn(rex::dbsp::NodeId) -> Node,
    batch: impl Fn(&BTreeRelation) -> BTreeRelation,
    steps: &[Vec<Change>],
) -> proptest::test_runner::TestCaseResult {
    let mut circuit = Circuit::new();
    let input = circuit.input(key_a());
    let out = circuit.add_node(node(input));
    circuit.set_output("out", out);

    let mut oracle_in = BTreeRelation::new();
    for changes in steps {
        circuit.step(&to_tx(&key_a(), changes));
        integrate(&mut oracle_in, changes);
        prop_assert_eq!(circuit.integral(out), &batch(&oracle_in));
    }
    Ok(())
}

/// Drive a two-input operator, with both inputs changing in the same
/// transaction (this exercises the δL·δR cross-term of the bilinear rules):
/// after every step, the node's integral must equal `batch` over the
/// integrated inputs.
fn check_binary(
    node: impl Fn(NodeId, NodeId) -> Node,
    batch: impl Fn(&BTreeRelation, &BTreeRelation) -> BTreeRelation,
    steps_a: &[Vec<Change>],
    steps_b: &[Vec<Change>],
) -> proptest::test_runner::TestCaseResult {
    let mut circuit = Circuit::new();
    let a = circuit.input(key_a());
    let b = circuit.input(key_b());
    let out = circuit.add_node(node(a, b));
    circuit.set_output("out", out);

    let mut oracle_a = BTreeRelation::new();
    let mut oracle_b = BTreeRelation::new();
    let empty: Vec<Change> = vec![];
    for i in 0..steps_a.len().max(steps_b.len()) {
        let ca = steps_a.get(i).unwrap_or(&empty);
        let cb = steps_b.get(i).unwrap_or(&empty);
        let mut tx = to_tx(&key_a(), ca);
        for &(l, r, w) in cb {
            tx.push(key_b(), int(l), int(r), w);
        }
        circuit.step(&tx);
        integrate(&mut oracle_a, ca);
        integrate(&mut oracle_b, cb);
        prop_assert_eq!(circuit.integral(out), &batch(&oracle_a, &oracle_b));
    }
    Ok(())
}

proptest! {
    #[test]
    fn inverse_matches_batch(steps in tx_seq()) {
        check_unary(Node::Inverse, |r| algebra::inverse(r), &steps)?;
    }

    #[test]
    fn filter_matches_batch(steps in tx_seq()) {
        let pred = Pred::Cmp(CmpOp::Gt, Lit::Int(2));
        check_unary(
            move |a| Node::Filter(a, pred.clone()),
            |r| algebra::filter_right(r, |v| matches!(v, Value::Int(n) if *n > 2)),
            &steps,
        )?;
    }

    #[test]
    fn inrel_matches_batch(steps in tx_seq()) {
        let set: BTreeSet<Value> = [int(1), int(3)].into();
        let batch_set = set.clone();
        check_unary(
            move |a| Node::InRel(a, set.clone()),
            move |r| {
                // Batch oracle mirroring interp.rs's InRel arm: coreflexive on
                // the left key for rows whose right value is in the set.
                let mut out = BTreeRelation::new();
                for (l, r_val, w) in r.iter() {
                    if batch_set.contains(&r_val) {
                        out.add(l.clone(), l, w);
                    }
                }
                out
            },
            &steps,
        )?;
    }

    #[test]
    fn map_const_matches_batch(steps in tx_seq()) {
        let val = Value::Money(999);
        let batch_val = val.clone();
        check_unary(
            move |a| Node::MapConst(a, val.clone()),
            move |r| {
                // Batch oracle mirroring interp.rs's Const arm: rewrite the
                // right column to the constant, per left row.
                let mut out = BTreeRelation::new();
                for (l, _, w) in r.iter() {
                    out.add(l, batch_val.clone(), w);
                }
                out
            },
            &steps,
        )?;
    }

    #[test]
    fn union_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_binary(Node::Union, |r, s| algebra::union(r, s), &steps_a, &steps_b)?;
    }

    #[test]
    fn compose_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_binary(
            |l, r| Node::Compose { l, r, linv: BTreeRelation::new() },
            |r, s| algebra::compose(r, s),
            &steps_a,
            &steps_b,
        )?;
    }

    #[test]
    fn fork_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_binary(
            |l, r| Node::CoKeyed { l, r, f: CoKeyedFn::Fork },
            |r, s| algebra::fork(r, s),
            &steps_a,
            &steps_b,
        )?;
    }

    #[test]
    fn mul_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_binary(
            |l, r| Node::CoKeyed { l, r, f: CoKeyedFn::Mul { money: false } },
            |r, s| algebra::value_join(r, s, |x, y| rex::eval::interp::mul_values(x, y, false)),
            &steps_a,
            &steps_b,
        )?;
    }

    #[test]
    fn concat_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_binary(
            |l, r| Node::CoKeyed { l, r, f: CoKeyedFn::Concat },
            |r, s| algebra::value_join(r, s, rex::eval::interp::concat_values),
            &steps_a,
            &steps_b,
        )?;
    }

    #[test]
    fn bincompare_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_binary(
            |l, r| Node::CoKeyed { l, r, f: CoKeyedFn::Compare(CmpOp::Gt) },
            |ra, rb| {
                // Batch oracle mirroring interp.rs's BinCompare arm: a
                // coreflexive on the shared key, weights bilinear.
                let mut out = BTreeRelation::new();
                for k in ra.domain() {
                    for (va, wa) in ra.row(&k) {
                        for (vb, wb) in rb.row(&k) {
                            if rex::eval::interp::compare_values(CmpOp::Gt, &va, &vb) {
                                out.add(k.clone(), k.clone(), wa * wb);
                            }
                        }
                    }
                }
                out
            },
            &steps_a,
            &steps_b,
        )?;
    }

    #[test]
    fn compose_chain_matches_batch(
        steps_a in tx_seq(),
        steps_b in tx_seq(),
        steps_c in tx_seq(),
    ) {
        // A stateful node fed by a stateful node: (A . B) . C, the shape every
        // multi-hop field path lowers to.
        let key_c = InputKey::Field(SortId(0), "c".to_string());
        let mut circuit = Circuit::new();
        let a = circuit.input(key_a());
        let b = circuit.input(key_b());
        let c = circuit.input(key_c.clone());
        let ab = circuit.add_node(Node::Compose { l: a, r: b, linv: BTreeRelation::new() });
        let abc = circuit.add_node(Node::Compose { l: ab, r: c, linv: BTreeRelation::new() });
        circuit.set_output("out", abc);

        let mut oracle_a = BTreeRelation::new();
        let mut oracle_b = BTreeRelation::new();
        let mut oracle_c = BTreeRelation::new();
        let empty: Vec<Change> = vec![];
        let len = steps_a.len().max(steps_b.len()).max(steps_c.len());
        for i in 0..len {
            let ca = steps_a.get(i).unwrap_or(&empty);
            let cb = steps_b.get(i).unwrap_or(&empty);
            let cc = steps_c.get(i).unwrap_or(&empty);
            let mut tx = Transaction::new();
            for (key, changes) in [(key_a(), ca), (key_b(), cb), (key_c.clone(), cc)] {
                for &(l, r, w) in changes {
                    tx.push(key.clone(), int(l), int(r), w);
                }
            }
            circuit.step(&tx);
            integrate(&mut oracle_a, ca);
            integrate(&mut oracle_b, cb);
            integrate(&mut oracle_c, cc);
            let batch = algebra::compose(&algebra::compose(&oracle_a, &oracle_b), &oracle_c);
            prop_assert_eq!(circuit.integral(abc), &batch);
        }
    }

    #[test]
    fn view_deltas_sum_to_integral(steps in tx_seq()) {
        // The per-step deltas a view reports must themselves integrate to the
        // view's full contents.
        let mut circuit = Circuit::new();
        let input = circuit.input(key_a());
        let out = circuit.add_node(Node::Inverse(input));
        circuit.set_output("out", out);

        let mut summed = BTreeRelation::new();
        for changes in &steps {
            let result = circuit.step(&to_tx(&key_a(), changes));
            for (l, r, w) in result.view_deltas["out"].iter() {
                summed.add(l, r, w);
            }
        }
        prop_assert_eq!(&summed, circuit.view("out").unwrap());
    }
}

#[test]
fn const_singleton_fires_exactly_once() {
    let mut circuit = Circuit::new();
    let c = circuit.add_node(Node::ConstSingleton { value: Value::Atom("west".into()), fired: false });
    circuit.set_output("c", c);

    let first = circuit.step(&Transaction::new());
    assert_eq!(first.view_deltas["c"].to_sorted_vec(), vec![(
        Value::Atom("west".into()),
        Value::Atom("west".into()),
        1,
    )]);

    let second = circuit.step(&Transaction::new());
    assert!(second.view_deltas["c"].is_empty());
    // Integral stays the singleton forever.
    assert_eq!(circuit.view("c").unwrap().len(), 1);
}

#[test]
fn data_before_view_is_kept() {
    // A transaction may target a base table no node reads yet; the input node
    // is created on the fly and its integral retained for later backfill.
    let mut circuit = Circuit::new();
    let mut tx = Transaction::new();
    tx.push(key_a(), int(1), int(10), 1);
    circuit.step(&tx);
    let integral = circuit.input_integral(&key_a()).unwrap();
    assert_eq!(integral.to_sorted_vec(), vec![(int(1), int(10), 1)]);
}

#[test]
fn compose_cross_term_when_both_inputs_change_together() {
    // Both inputs receive their *first* data in one transaction: the entire
    // output comes from the δL·δR cross-term. Using δL·I(S) instead of δL·S'
    // would produce nothing — the classic off-by-one in the bilinear rule.
    let mut circuit = Circuit::new();
    let a = circuit.input(key_a());
    let b = circuit.input(key_b());
    let out = circuit.add_node(Node::Compose { l: a, r: b, linv: BTreeRelation::new() });
    circuit.set_output("out", out);

    let mut tx = Transaction::new();
    tx.push(key_a(), int(1), int(2), 1);
    tx.push(key_b(), int(2), int(3), 1);
    let result = circuit.step(&tx);
    assert_eq!(result.view_deltas["out"].to_sorted_vec(), vec![(int(1), int(3), 1)]);

    // And the mirrored case under retraction: retract both sides at once; the
    // join row must disappear exactly once (weight -1, not -2 or 0).
    let mut retract = Transaction::new();
    retract.push(key_a(), int(1), int(2), -1);
    retract.push(key_b(), int(2), int(3), -1);
    let result = circuit.step(&retract);
    assert_eq!(result.view_deltas["out"].to_sorted_vec(), vec![(int(1), int(3), -1)]);
    assert!(circuit.view("out").unwrap().is_empty());
}

#[test]
fn cancelling_weights_prune_from_integral() {
    let mut circuit = Circuit::new();
    let input = circuit.input(key_a());
    let out = circuit.add_node(Node::Inverse(input));
    circuit.set_output("out", out);

    let mut tx = Transaction::new();
    tx.push(key_a(), int(1), int(10), 1);
    circuit.step(&tx);
    assert_eq!(circuit.view("out").unwrap().len(), 1);

    // Retract it: the view must return to empty (zero-weight rows pruned).
    let mut retract = Transaction::new();
    retract.push(key_a(), int(1), int(10), -1);
    let result = circuit.step(&retract);
    assert_eq!(result.view_deltas["out"].to_sorted_vec(), vec![(int(10), int(1), -1)]);
    assert!(circuit.view("out").unwrap().is_empty());
}
