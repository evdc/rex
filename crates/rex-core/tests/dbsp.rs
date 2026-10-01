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
use rex::eval::algebra::Agg;
use rex::eval::relation::{BTreeRelation, BinaryRelation};
use rex::eval::{Value, algebra, intern};
use rex::types::ty::SortId;
use rex::types::typed::{AggKind, Lit, Pred, Total};
use std::collections::BTreeMap;
use std::collections::BTreeSet;

fn compose_node(l: NodeId, r: NodeId) -> Node {
    Node::Compose { l, r, linv: BTreeRelation::new() }
}

fn semijoin_node(l: NodeId, r: NodeId) -> Node {
    Node::Semijoin { l, r, linv: BTreeRelation::new() }
}

fn antijoin_node(l: NodeId, r: NodeId) -> Node {
    Node::Antijoin { l, r, linv: BTreeRelation::new() }
}

fn agg_node(kind: AggKind) -> impl Fn(NodeId) -> Node {
    move |input| Node::Aggregate { input, kind, money: false, total: Total::No, seeded: false, st: BTreeMap::new() }
}

/// Batch antijoin oracle, mirroring interp.rs: `A − A[B]`.
fn batch_antijoin(a: &BTreeRelation, b: &BTreeRelation) -> BTreeRelation {
    let sj = algebra::semijoin(a, b);
    let mut out = a.clone();
    for (l, r, w) in sj.iter() {
        out.add(l, r, -w);
    }
    out
}

fn int(n: i64) -> Value {
    Value::Int(n)
}

fn key_a() -> InputKey {
    InputKey::Field(SortId(0), intern("a"))
}

fn key_b() -> InputKey {
    InputKey::Field(SortId(0), intern("b"))
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
        tx.push(*key, int(l), int(r), w);
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

/// Drive `Proj` over a `Fork` of two changing inputs — pairs are only ever
/// built by fork, so this is the shape every projection consumes. After every
/// step the integral must equal the batch `proj(fork(a, b))`.
fn check_proj(
    side: rex::ast::ProjSide,
    steps_a: &[Vec<Change>],
    steps_b: &[Vec<Change>],
) -> proptest::test_runner::TestCaseResult {
    let mut circuit = Circuit::new();
    let a = circuit.input(key_a());
    let b = circuit.input(key_b());
    let fork = circuit.add_node(Node::CoKeyed { l: a, r: b, f: CoKeyedFn::Fork });
    let out = circuit.add_node(Node::Proj(fork, side));
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
        let batch = algebra::proj(&algebra::fork(&oracle_a, &oracle_b), side);
        prop_assert_eq!(circuit.integral(out), &batch);
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
        check_binary(compose_node, |r, s| algebra::compose(r, s), &steps_a, &steps_b)?;
    }

    #[test]
    fn semijoin_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_binary(semijoin_node, |r, s| algebra::semijoin(r, s), &steps_a, &steps_b)?;
    }

    #[test]
    fn antijoin_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_binary(antijoin_node, batch_antijoin, &steps_a, &steps_b)?;
    }

    #[test]
    fn intersect_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_binary(Node::Intersect, |r, s| algebra::intersect(r, s), &steps_a, &steps_b)?;
    }

    #[test]
    fn distinct_matches_batch(steps in tx_seq()) {
        check_unary(Node::Distinct, |r| algebra::distinct(r), &steps)?;
    }

    #[test]
    fn agg_sum_matches_batch(steps in tx_seq()) {
        check_unary(agg_node(AggKind::Sum), |r| algebra::aggregate(r, Agg::Sum, false), &steps)?;
    }

    #[test]
    fn agg_count_matches_batch(steps in tx_seq()) {
        check_unary(agg_node(AggKind::Count), |r| algebra::aggregate(r, Agg::Count, false), &steps)?;
    }

    #[test]
    fn agg_avg_matches_batch(steps in tx_seq()) {
        check_unary(agg_node(AggKind::Avg), |r| algebra::aggregate(r, Agg::Avg, false), &steps)?;
    }

    #[test]
    fn agg_min_matches_batch(steps in tx_seq()) {
        check_unary(agg_node(AggKind::Min), |r| algebra::aggregate(r, Agg::Min, false), &steps)?;
    }

    #[test]
    fn agg_max_matches_batch(steps in tx_seq()) {
        check_unary(agg_node(AggKind::Max), |r| algebra::aggregate(r, Agg::Max, false), &steps)?;
    }

    #[test]
    fn agg_then_filter_matches_batch(steps in tx_seq()) {
        // The spec12 `custspend where > 30` shape: a group aggregate feeding a
        // threshold filter — retract/assert deltas must cross the threshold
        // cleanly in both directions.
        let pred = Pred::Cmp(CmpOp::Gt, Lit::Int(2));
        let mut circuit = Circuit::new();
        let input = circuit.input(key_a());
        let agg = circuit.add_node(Node::Aggregate {
            input,
            kind: AggKind::Sum,
            money: false,
            total: Total::No,
            seeded: false,
            st: BTreeMap::new(),
        });
        let out = circuit.add_node(Node::Filter(agg, pred));
        circuit.set_output("out", out);

        let mut oracle_in = BTreeRelation::new();
        for changes in &steps {
            circuit.step(&to_tx(&key_a(), changes));
            integrate(&mut oracle_in, changes);
            let batch = algebra::filter_right(
                &algebra::aggregate(&oracle_in, Agg::Sum, false),
                |v| matches!(v, Value::Int(n) if *n > 2),
            );
            prop_assert_eq!(circuit.integral(out), &batch);
        }
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
    fn proj_fst_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_proj(rex::ast::ProjSide::Fst, &steps_a, &steps_b)?;
    }

    #[test]
    fn proj_snd_matches_batch(steps_a in tx_seq(), steps_b in tx_seq()) {
        check_proj(rex::ast::ProjSide::Snd, &steps_a, &steps_b)?;
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
        let key_c = InputKey::Field(SortId(0), intern("c"));
        let mut circuit = Circuit::new();
        let a = circuit.input(key_a());
        let b = circuit.input(key_b());
        let c = circuit.input(key_c);
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
            for (key, changes) in [(key_a(), ca), (key_b(), cb), (key_c, cc)] {
                for &(l, r, w) in changes {
                    tx.push(key, int(l), int(r), w);
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
    let c = circuit.add_node(Node::ConstSingleton { value: Value::atom("west"), fired: false });
    circuit.set_output("c", c);

    let first = circuit.step(&Transaction::new());
    assert_eq!(first.view_deltas["c"].to_sorted_vec(), vec![(
        Value::atom("west"),
        Value::atom("west"),
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

// --- adversarial cases for the non-linear tier ------------------------------
//
// The property net above covers these statistically; the cases below pin the
// narrow mechanisms by name so a regression is immediately legible.

/// Harness: a two-input circuit, stepped with explicit per-side changes.
struct Pair {
    circuit: Circuit,
    out: NodeId,
}

impl Pair {
    fn new(node: impl Fn(NodeId, NodeId) -> Node) -> Pair {
        let mut circuit = Circuit::new();
        let a = circuit.input(key_a());
        let b = circuit.input(key_b());
        let out = circuit.add_node(node(a, b));
        circuit.set_output("out", out);
        Pair { circuit, out }
    }

    /// Apply changes to both sides in ONE transaction; return the view delta.
    fn step(&mut self, a: &[Change], b: &[Change]) -> Vec<(Value, Value, i64)> {
        let mut tx = to_tx(&key_a(), a);
        for &(l, r, w) in b {
            tx.push(key_b(), int(l), int(r), w);
        }
        self.circuit.step(&tx).view_deltas["out"].to_sorted_vec()
    }

    fn contents(&self) -> Vec<(Value, Value, i64)> {
        self.circuit.integral(self.out).to_sorted_vec()
    }
}

fn triples(entries: &[(i64, i64, i64)]) -> Vec<(Value, Value, i64)> {
    entries.iter().map(|&(l, r, w)| (int(l), int(r), w)).collect()
}

#[test]
fn semijoin_membership_flips() {
    let mut p = Pair::new(semijoin_node);
    // L gets rows keyed at 2 before R knows the key: nothing passes.
    assert!(p.step(&[(1, 2, 3)], &[]).is_empty());

    // R gains its first row at 2: membership flips ON, L's slice replays with
    // its full weight (3).
    assert_eq!(p.step(&[], &[(2, 9, 1)]), triples(&[(1, 2, 3)]));

    // A second R row at 2, then one retracted: membership never flips, no output.
    assert!(p.step(&[], &[(2, 8, 1)]).is_empty());
    assert!(p.step(&[], &[(2, 8, -1)]).is_empty());

    // Last R row retracted: flips OFF, the slice retracts.
    assert_eq!(p.step(&[], &[(2, 9, -1)]), triples(&[(1, 2, -3)]));
    assert!(p.contents().is_empty());
}

#[test]
fn semijoin_cancelling_r_rows_do_not_flip() {
    let mut p = Pair::new(semijoin_node);
    p.step(&[(1, 2, 1)], &[]);
    // +1 and −1 at the same R key in one transaction: net membership change is
    // zero; the merged `has_left` must see through the cancellation.
    assert!(p.step(&[], &[(2, 9, 1), (2, 9, -1)]).is_empty());
    assert!(p.contents().is_empty());
}

#[test]
fn semijoin_delta_l_and_flip_in_same_transaction() {
    let mut p = Pair::new(semijoin_node);
    p.step(&[(1, 2, 1)], &[]);
    // New L row at 2 AND R's first row at 2 arrive together: the old L row
    // comes in via the flip term, the new one via δL ⋉ M' — exactly once each.
    assert_eq!(p.step(&[(5, 2, 1)], &[(2, 9, 1)]), triples(&[(1, 2, 1), (5, 2, 1)]));
}

#[test]
fn antijoin_first_and_last_match_with_multiplicity() {
    let mut p = Pair::new(antijoin_node);
    // A customer with weight-3 presence and no match: fully in the antijoin.
    assert_eq!(p.step(&[(1, 2, 3)], &[]), triples(&[(1, 2, 3)]));

    // First match appears: rows leave with the SAME weight (the §6 weight-
    // correctness property — subtracting the semijoin, not the raw image).
    assert_eq!(p.step(&[], &[(2, 9, 1)]), triples(&[(1, 2, -3)]));
    assert!(p.contents().is_empty());

    // Last match leaves: rows return.
    assert_eq!(p.step(&[], &[(2, 9, -1)]), triples(&[(1, 2, 3)]));
}

#[test]
fn intersect_min_crossing_both_directions() {
    let mut p = Pair::new(Node::Intersect);
    p.step(&[(1, 1, 2)], &[(1, 1, 5)]);
    assert_eq!(p.contents(), triples(&[(1, 1, 2)])); // min(2,5)

    // Left grows past right: min switches sides, delta is the difference.
    assert_eq!(p.step(&[(1, 1, 4)], &[]), triples(&[(1, 1, 3)])); // min 2 -> 5
    // Right shrinks below left: min follows it down.
    assert_eq!(p.step(&[], &[(1, 1, -4)]), triples(&[(1, 1, -4)])); // min 5 -> 1
    assert_eq!(p.contents(), triples(&[(1, 1, 1)]));
}

#[test]
fn intersect_negative_weight_on_one_side() {
    let mut p = Pair::new(Node::Intersect);
    // min(-1, 2) = -1: intersect must not clamp.
    p.step(&[(1, 1, -1)], &[(1, 1, 2)]);
    assert_eq!(p.contents(), triples(&[(1, 1, -1)]));
}

#[test]
fn distinct_weight_transitions() {
    let mut circuit = Circuit::new();
    let input = circuit.input(key_a());
    let out = circuit.add_node(Node::Distinct(input));
    circuit.set_output("out", out);
    let mut step = |changes: &[Change]| -> Vec<(Value, Value, i64)> {
        circuit.step(&to_tx(&key_a(), changes)).view_deltas["out"].to_sorted_vec()
    };

    // 0 -> 2: appears once.
    assert_eq!(step(&[(1, 1, 2)]), triples(&[(1, 1, 1)]));
    // 2 -> 1: still present, no output delta.
    assert!(step(&[(1, 1, -1)]).is_empty());
    // 1 -> 0: retraction.
    assert_eq!(step(&[(1, 1, -1)]), triples(&[(1, 1, -1)]));
    // 0 -> -1: still absent (clamp of a negative is 0), no delta.
    assert!(step(&[(1, 1, -1)]).is_empty());
    // -1 -> 0: still absent.
    assert!(step(&[(1, 1, 1)]).is_empty());
    // 0 -> 1: appears again.
    assert_eq!(step(&[(1, 1, 1)]), triples(&[(1, 1, 1)]));
}

/// Harness for one-input aggregate cases.
fn agg_circuit(kind: AggKind) -> (Circuit, NodeId) {
    let mut circuit = Circuit::new();
    let input = circuit.input(key_a());
    let out = circuit.add_node(Node::Aggregate { input, kind, money: false, total: Total::No, seeded: false, st: BTreeMap::new() });
    circuit.set_output("out", out);
    (circuit, out)
}

#[test]
fn min_retracting_current_minimum_forces_rescan() {
    let (mut circuit, out) = agg_circuit(AggKind::Min);
    let mut tx = Transaction::new();
    tx.push(key_a(), int(1), int(3), 1);
    tx.push(key_a(), int(1), int(7), 1);
    circuit.step(&tx);
    assert_eq!(circuit.integral(out).to_sorted_vec(), triples(&[(1, 3, 1)]));

    // Retract the minimum itself: the survivor (7) must be found by rescan.
    let mut retract = Transaction::new();
    retract.push(key_a(), int(1), int(3), -1);
    let result = circuit.step(&retract);
    assert_eq!(
        result.view_deltas["out"].to_sorted_vec(),
        triples(&[(1, 3, -1), (1, 7, 1)])
    );
    assert_eq!(circuit.integral(out).to_sorted_vec(), triples(&[(1, 7, 1)]));
}

#[test]
fn avg_key_disappears_when_group_empties() {
    let (mut circuit, out) = agg_circuit(AggKind::Avg);
    let mut tx = Transaction::new();
    tx.push(key_a(), int(1), int(4), 1);
    circuit.step(&tx);
    assert_eq!(
        circuit.integral(out).to_sorted_vec(),
        vec![(int(1), Value::Money(4), 1)]
    );

    let mut retract = Transaction::new();
    retract.push(key_a(), int(1), int(4), -1);
    circuit.step(&retract);
    assert!(circuit.integral(out).is_empty());
}

#[test]
fn mixed_sign_group_is_present_with_zero_count() {
    // Two values whose weights are +1 and -1: the group is nonempty (two
    // nonzero entries) but count == 0. Batch emits Sum(0) and Count(0), and
    // skips Avg — the incremental presence rule must match all three.
    let changes: &[Change] = &[(1, 3, 1), (1, 5, -1)];

    let (mut sum_c, sum_out) = agg_circuit(AggKind::Sum);
    sum_c.step(&to_tx(&key_a(), changes));
    assert_eq!(sum_c.integral(sum_out).to_sorted_vec(), triples(&[(1, -2, 1)]));

    let (mut count_c, count_out) = agg_circuit(AggKind::Count);
    count_c.step(&to_tx(&key_a(), changes));
    assert_eq!(count_c.integral(count_out).to_sorted_vec(), triples(&[(1, 0, 1)]));

    let (mut avg_c, avg_out) = agg_circuit(AggKind::Avg);
    avg_c.step(&to_tx(&key_a(), changes));
    assert!(avg_c.integral(avg_out).is_empty());
}

#[test]
fn sum_crossing_downstream_threshold_both_directions() {
    // spec12's `custspend where > 30` shape, minimal: Sum feeding Filter(> 2).
    let mut circuit = Circuit::new();
    let input = circuit.input(key_a());
    let agg = circuit.add_node(Node::Aggregate {
        input,
        kind: AggKind::Sum,
        money: false,
        total: Total::No,
        seeded: false,
        st: BTreeMap::new(),
    });
    let out = circuit.add_node(Node::Filter(agg, Pred::Cmp(CmpOp::Gt, Lit::Int(2))));
    circuit.set_output("out", out);
    let mut step = |changes: &[Change]| -> Vec<(Value, Value, i64)> {
        circuit.step(&to_tx(&key_a(), changes)).view_deltas["out"].to_sorted_vec()
    };

    // Sum = 2: below threshold, invisible.
    assert!(step(&[(1, 2, 1)]).is_empty());
    // Sum = 4: crosses up — appears with the new value only.
    assert_eq!(step(&[(1, 2, 1)]), triples(&[(1, 4, 1)]));
    // Sum = 6: stays above — clean retract/assert of the changed value.
    assert_eq!(step(&[(1, 2, 1)]), triples(&[(1, 4, -1), (1, 6, 1)]));
    // Sum = 2: crosses back down — the old value retracts, nothing replaces it.
    assert_eq!(step(&[(1, 2, -2)]), triples(&[(1, 6, -1)]));
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

// --- recursion (§8): the fix region vs. a batch Kleene oracle ---------------

/// Batch transitive-closure oracle: Kleene iteration of
/// `path = distinct(edge | edge . path)` — the same equation the fix region
/// solves, computed entirely with the batch algebra.
fn batch_closure(edge: &BTreeRelation) -> BTreeRelation {
    let mut path = BTreeRelation::new();
    loop {
        let next = algebra::distinct(&algebra::union(
            edge,
            &algebra::compose(edge, &path),
        ));
        if next == path {
            return path;
        }
        path = next;
    }
}

/// An engine whose circuit holds the recursive `path` view over an
/// (initially empty) Edge entity.
fn closure_engine() -> rex::dbsp::Engine {
    let src = "\
entity Node { name: Text }
entity Edge { src: NodeID, dst: NodeID }
let srcof : Edge -> Node = .src
let dstof : Edge -> Node = .dst
let edge : Node -> Node = dstof by srcof
let recursive path : Node -> Node = edge | edge . path
";
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty());
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let typed = checked.elaborated.expect("elaborated");

    let mut engine = rex::dbsp::Engine::new();
    let values = std::collections::HashMap::new();
    for stmt in &typed.stmts {
        match stmt {
            rex::types::typed::TStmt::Let { name: Some(name), body } => {
                engine.add_view(name, body, &values);
            }
            rex::types::typed::TStmt::LetRec { bindings } => {
                engine.add_view_group(bindings, &values);
            }
            _ => {}
        }
    }
    engine
}

proptest! {
    /// Feed a random insert/retract history over a tiny node domain (weights
    /// stay faithful to the language: an edge entity exists once or not at
    /// all — `new` inserts +1, retraction negates what exists; net-negative
    /// base rows are unreachable, and monotone-in-X fixpoints require them to
    /// be) and after EVERY step assert the engine's `path` equals the batch
    /// closure of the integrated edge relation.
    #[test]
    fn fix_region_matches_batch_closure(seq in prop::collection::vec(
        prop::collection::vec((0u64..6, 0u64..4, 0u64..4, prop::bool::ANY), 0..4),
        1..10,
    )) {
        let mut engine = closure_engine();
        let node_sort = SortId(0);
        let edge_sort = SortId(1);
        let src_key = InputKey::Field(edge_sort, intern("src"));
        let dst_key = InputKey::Field(edge_sort, intern("dst"));
        let id_key = InputKey::Identity(edge_sort);
        // Live edge entities: id -> (src, dst).
        let mut live: BTreeMap<u64, (u64, u64)> = BTreeMap::new();

        for step in &seq {
            let mut tx = Transaction::new();
            for &(e, u, v, insert) in step {
                let eid = Value::Id(edge_sort, e);
                if insert {
                    if live.contains_key(&e) {
                        continue; // entity ids are unique
                    }
                    live.insert(e, (u, v));
                    tx.push(id_key, eid.clone(), eid.clone(), 1);
                    tx.push(src_key, eid.clone(), Value::Id(node_sort, u), 1);
                    tx.push(dst_key, eid, Value::Id(node_sort, v), 1);
                } else if let Some((u, v)) = live.remove(&e) {
                    // Retract exactly the rows the entity holds.
                    tx.push(id_key, eid.clone(), eid.clone(), -1);
                    tx.push(src_key, eid.clone(), Value::Id(node_sort, u), -1);
                    tx.push(dst_key, eid, Value::Id(node_sort, v), -1);
                }
            }
            engine.circuit.step(&tx);

            let src_int = engine.circuit.input_integral(&src_key).map(|v| v.to_relation()).unwrap_or_default();
            let dst_int = engine.circuit.input_integral(&dst_key).map(|v| v.to_relation()).unwrap_or_default();
            let edge = algebra::compose(&algebra::inverse(&src_int), &dst_int);
            prop_assert_eq!(
                engine.circuit.view("edge").map(|v| v.to_relation()).unwrap_or_default(),
                edge.clone(),
                "edge view diverged"
            );
            prop_assert_eq!(
                engine.circuit.view("path").map(|v| v.to_relation()).unwrap_or_default(),
                batch_closure(&edge),
                "path fixpoint diverged from batch closure"
            );
        }
    }
}

// --- P-2: integrals only where something reads them ------------------------

/// `~A . B` filtered twice: the inverse feeds compose's *left* side (read
/// only as a delta — `linv` stands in for its integral), and the compose and
/// first filter feed only linear parents. Neither keeps state; B, probed as compose's right
/// side, does; and the view is still exact, because it keeps its own.
#[test]
fn linear_nodes_keep_no_integral_unless_read() {
    let mut circuit = Circuit::new();
    let a = circuit.input(key_a());
    let b = circuit.input(key_b());
    let inv = circuit.add_node(Node::Inverse(a));
    let ab = circuit.add_node(compose_node(inv, b));
    let filt = circuit.add_node(Node::Filter(ab, Pred::Cmp(CmpOp::Gt, Lit::Int(0))));
    let view = circuit.add_node(Node::Filter(filt, Pred::Cmp(CmpOp::Gt, Lit::Int(5))));
    circuit.set_output("out", view);

    assert!(circuit.keeps_integral(a) && circuit.keeps_integral(b), "inputs always keep theirs");
    assert!(!circuit.keeps_integral(inv), "compose's left side is read as a delta only");
    assert!(!circuit.keeps_integral(ab) && !circuit.keeps_integral(filt), "linear chain");
    assert!(circuit.keeps_integral(view), "views are read back");

    let mut tx = Transaction::new();
    tx.push(key_a(), int(1), int(2), 1);
    tx.push(key_b(), int(1), int(9), 1);
    tx.push(key_b(), int(1), int(3), 1);
    circuit.step(&tx);
    assert_eq!(circuit.view("out").unwrap().to_sorted_vec(), vec![(int(2), int(9), 1)]);
}

/// P-2b: an equal node is shared, even after it has stepped without an
/// integral, when it is stateless. A later view's backfill then recomputes
/// it from its children's history, and a view that names it outright makes
/// it start keeping an integral, rebuilt by that backfill.
#[test]
fn stateless_nodes_are_shared_and_replayed_after_stepping() {
    let mut circuit = Circuit::new();
    let a = circuit.input(key_a());
    let b = circuit.input(key_b());
    let inv = circuit.add_node(Node::Inverse(a));
    let filt = circuit.add_node(Node::Filter(inv, Pred::Cmp(CmpOp::Gt, Lit::Int(1))));
    let first = circuit.add_node(compose_node(filt, b));
    circuit.set_output("first", first);
    circuit.backfill(0, &["first"]);
    assert!(!circuit.keeps_integral(inv) && !circuit.keeps_integral(filt));

    let mut tx = Transaction::new();
    tx.push(key_a(), int(1), int(2), 1);
    tx.push(key_a(), int(5), int(3), 1);
    tx.push(key_b(), int(1), int(9), 1);
    tx.push(key_b(), int(5), int(7), 1);
    circuit.step(&tx);

    // Built again after stepping: the same nodes, not copies.
    let mark = circuit.node_count();
    let inv2 = circuit.add_node(Node::Inverse(a));
    let filt2 = circuit.add_node(Node::Filter(inv2, Pred::Cmp(CmpOp::Gt, Lit::Int(1))));
    assert_eq!((inv2, filt2), (inv, filt));
    // A new parent reads `filt` below the floor; a view names it outright.
    let again = circuit.add_node(Node::Union(filt2, filt2));
    circuit.set_output("again", again);
    circuit.set_output("filt", filt2);
    assert!(circuit.keeps_integral(filt));
    let result = circuit.backfill(mark, &["again", "filt"]);

    let expected_filt = vec![(int(3), int(5), 1)]; // `> 1` tests the right column
    assert_eq!(circuit.view("filt").unwrap().to_sorted_vec(), expected_filt);
    assert_eq!(result.view_deltas["filt"].to_sorted_vec(), expected_filt);
    let doubled: Vec<_> = expected_filt.iter().map(|(l, r, w)| (l.clone(), r.clone(), 2 * w)).collect();
    assert_eq!(circuit.view("again").unwrap().to_sorted_vec(), doubled);
    // The replay touched no view that already held its history.
    assert_eq!(circuit.view("first").unwrap().to_sorted_vec(), vec![(int(3), int(7), 1)]);

    // And all of it stays maintained.
    let mut tx = Transaction::new();
    tx.push(key_a(), int(1), int(2), -1);
    tx.push(key_a(), int(6), int(4), 1);
    let step = circuit.step(&tx);
    let expected_filt = vec![(int(3), int(5), 1), (int(4), int(6), 1)];
    assert_eq!(circuit.view("filt").unwrap().to_sorted_vec(), expected_filt);
    assert_eq!(step.view_deltas["filt"].to_sorted_vec(), vec![(int(4), int(6), 1)]);
    let doubled: Vec<_> = expected_filt.iter().map(|(l, r, w)| (l.clone(), r.clone(), 2 * w)).collect();
    assert_eq!(circuit.view("again").unwrap().to_sorted_vec(), doubled);
}

/// A node with private state is replayed too: resetting it and running its
/// kernel over the full history rebuilds that state (here a compose's index
/// of its left side, and an aggregate's accumulators) as it was, so both the
/// view that shares it later and the one that had it all along stay right.
#[test]
fn stateful_nodes_are_replayed_with_their_state() {
    let mut circuit = Circuit::new();
    let a = circuit.input(key_a());
    let b = circuit.input(key_b());
    let ab = circuit.add_node(compose_node(a, b));
    let count = |ab| Node::Aggregate {
        input: ab,
        kind: AggKind::Count,
        money: false,
        total: Total::No,
        seeded: false,
        st: Default::default(),
    };
    let first = circuit.add_node(Node::Union(ab, ab));
    circuit.set_output("first", first);
    circuit.backfill(0, &["first"]);
    let mut tx = Transaction::new();
    tx.push(key_a(), int(1), int(2), 1);
    tx.push(key_b(), int(2), int(9), 1);
    circuit.step(&tx);
    assert!(!circuit.keeps_integral(ab));

    let mark = circuit.node_count();
    assert_eq!(circuit.add_node(compose_node(a, b)), ab);
    let n = circuit.add_node(count(ab));
    circuit.set_output("n", n);
    circuit.backfill(mark, &["n"]);
    assert_eq!(circuit.view("n").unwrap().to_sorted_vec(), vec![(int(1), int(1), 1)]);

    // `ab`'s left index was rebuilt, not doubled or lost: a new right row
    // joins exactly once, for both parents.
    let mut tx = Transaction::new();
    tx.push(key_b(), int(2), int(8), 1);
    circuit.step(&tx);
    assert_eq!(circuit.view("n").unwrap().to_sorted_vec(), vec![(int(1), int(2), 1)]);
    assert_eq!(circuit.view("first").unwrap().to_sorted_vec(), vec![(int(1), int(8), 2), (int(1), int(9), 2)]);
}

// --- P-4b: column integrals ---------------------------------------------------
//
// A column holds a functional id-keyed relation and demotes itself to a
// general one the first time a change would break that. Here columns get
// mostly well-behaved writes (set a field, delete a row) mixed with arbitrary
// ones, so they both stay columns for a while and then demote mid-history;
// every integral must equal the general relation throughout.

#[derive(Clone, Debug)]
enum ColOp {
    /// `−old/+new` at `id`, as `push_set` writes it.
    Set(u64, i64),
    /// Retract whatever `id` holds.
    Del(u64),
    /// Any triple at all (a second value, weight 2, a retraction of nothing).
    Raw(u64, i64, i64),
}

fn col_ops() -> impl Strategy<Value = Vec<Vec<ColOp>>> {
    let op = prop_oneof![
        6 => (0u64..6, 0i64..3).prop_map(|(i, v)| ColOp::Set(i, v)),
        2 => (0u64..6).prop_map(ColOp::Del),
        1 => (0u64..6, 0i64..3, -2i64..=2).prop_map(|(i, v, w)| ColOp::Raw(i, v, w)),
    ];
    prop::collection::vec(prop::collection::vec(op, 0..5), 1..14)
}

fn col_id(n: u64) -> Value {
    // Spread ids over several 1024-slot pages.
    Value::Id(SortId(0), n * 700)
}

/// The rows one step of `ops` writes to a table whose current value is `now`.
fn col_rows(now: &BTreeRelation, ops: &[ColOp]) -> Vec<(Value, Value, i64)> {
    let mut pending = now.clone();
    let mut rows = Vec::new();
    for op in ops {
        let mut step = Vec::new();
        match op {
            ColOp::Set(i, _) | ColOp::Del(i) => {
                for (old, w) in pending.row(&col_id(*i)) {
                    step.push((col_id(*i), old, -w));
                }
                if let ColOp::Set(_, v) = op {
                    step.push((col_id(*i), int(*v), 1));
                }
            }
            ColOp::Raw(i, v, w) => step.push((col_id(*i), int(*v), *w)),
        }
        for (l, r, w) in step {
            pending.add(l.clone(), r.clone(), w);
            rows.push((l, r, w));
        }
    }
    rows
}

proptest! {
    #[test]
    fn column_integral_matches_general(steps in col_ops()) {
        let mut col = rex::dbsp::Integral::empty(Some(SortId(0)));
        let mut oracle = BTreeRelation::new();
        for ops in &steps {
            let rows = col_rows(&oracle, ops);
            col.commit(&rex::dbsp::Batch::from_rows(rows.clone()));
            for (l, r, w) in rows {
                oracle.add(l, r, w);
            }
            prop_assert_eq!(&col, &oracle);
            prop_assert_eq!(col.to_sorted_vec(), oracle.to_sorted_vec());
        }
    }

    /// Kernels reading column integrals: a compose probing a column, a
    /// co-keyed op over two, a semijoin against the identity column, and a
    /// sum grouped by id — each against the batch oracle.
    #[test]
    fn kernels_over_columns_match_batch(sa in col_ops(), sb in col_ops(), sl in prop::collection::vec(prop::collection::vec((0u64..6, any::<bool>()), 0..4), 1..14)) {
        let (ka, kb, ki) = (key_a(), key_b(), InputKey::Identity(SortId(0)));
        let mut c = Circuit::new();
        let (a, b, ids) = (c.input(ka), c.input(kb), c.input(ki));
        let outs = [
            c.add_node(Node::CoKeyed { l: a, r: b, f: CoKeyedFn::Fork }),
            c.add_node(semijoin_node(a, ids)),
            c.add_node(compose_node(ids, a)),
            c.add_node(Node::MapConst(ids, Value::Unit)),
            c.add_node(agg_node(AggKind::Sum)(b)),
        ];
        for (i, o) in outs.iter().enumerate() {
            c.set_output(&format!("o{i}"), *o);
        }
        prop_assert!(c.integral(a).is_column() && c.integral(outs[3]).is_column());
        let (mut oa, mut ob, mut oi) = (BTreeRelation::new(), BTreeRelation::new(), BTreeRelation::new());
        let none = vec![];
        for i in 0..sa.len().max(sb.len()).max(sl.len()) {
            let ra = col_rows(&oa, sa.get(i).unwrap_or(&none));
            let rb = col_rows(&ob, sb.get(i).unwrap_or(&none));
            let mut tx = Transaction::new();
            for (key, rows, o) in [(ka, &ra, &mut oa), (kb, &rb, &mut ob)] {
                for (l, r, w) in rows {
                    tx.push(key, l.clone(), r.clone(), *w);
                    o.add(l.clone(), r.clone(), *w);
                }
            }
            for &(n, live) in sl.get(i).unwrap_or(&vec![]) {
                let id = col_id(n);
                let w = oi.weight(&id, &id);
                let dw = if live { 1 - w } else { -w };
                if dw != 0 {
                    tx.push(ki, id.clone(), id.clone(), dw);
                    oi.add(id.clone(), id, dw);
                }
            }
            c.step(&tx);
            prop_assert_eq!(c.integral(outs[0]), &algebra::fork(&oa, &ob));
            prop_assert_eq!(c.integral(outs[1]), &algebra::semijoin(&oa, &oi));
            prop_assert_eq!(c.integral(outs[2]), &algebra::compose(&oi, &oa));
            prop_assert_eq!(c.integral(outs[3]), &BTreeRelation::from_triples(oi.iter().map(|(l, _, w)| (l, Value::Unit, w))));
            prop_assert_eq!(c.integral(outs[4]), &algebra::aggregate(&ob, Agg::Sum, false));
        }
    }
}
