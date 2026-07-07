//! Evaluator tests: the BTree relation, the algebra, and the §12 end-to-end run.

use rex::eval::relation::{BTreeRelation, BinaryRelation};
use rex::eval::value::Value;
use rex::eval::{algebra, run};
use rex::parse;
use std::collections::BTreeMap;

fn int(n: i64) -> Value {
    Value::Int(n)
}

fn rel(triples: &[(i64, i64, i64)]) -> BTreeRelation {
    BTreeRelation::from_triples(triples.iter().map(|&(l, r, w)| (int(l), int(r), w)))
}

// --- BTreeRelation --------------------------------------------------------

#[test]
fn add_accumulates_weights_and_prunes_zero() {
    let mut r = BTreeRelation::new();
    r.add(int(1), int(2), 1);
    r.add(int(1), int(2), 2);
    assert_eq!(r.weight(&int(1), &int(2)), 3);
    assert_eq!(r.len(), 1);
    // A retraction back to zero prunes the entry and empties the domain.
    r.add(int(1), int(2), -3);
    assert_eq!(r.weight(&int(1), &int(2)), 0);
    assert_eq!(r.len(), 0);
    assert!(!r.has_left(&int(1)));
    assert_eq!(r.domain().count(), 0);
}

#[test]
fn row_and_domain() {
    let r = rel(&[(1, 10, 1), (1, 11, 1), (2, 20, 1)]);
    let mut row1: Vec<_> = r.row(&int(1)).collect();
    row1.sort();
    assert_eq!(row1, vec![(int(10), 1), (int(11), 1)]);
    let dom: Vec<_> = r.domain().collect();
    assert_eq!(dom, vec![int(1), int(2)]);
}

// --- algebra --------------------------------------------------------------

#[test]
fn compose_joins_on_middle_column() {
    let r = rel(&[(1, 2, 1)]);
    let s = rel(&[(2, 3, 1), (2, 4, 1)]);
    let out = algebra::compose(&r, &s);
    assert_eq!(out.to_sorted_vec(), vec![(int(1), int(3), 1), (int(1), int(4), 1)]);
}

#[test]
fn compose_multiplies_weights() {
    let r = rel(&[(1, 2, 2)]);
    let s = rel(&[(2, 3, 3)]);
    assert_eq!(algebra::compose(&r, &s).weight(&int(1), &int(3)), 6);
}

#[test]
fn semijoin_keeps_left_weights_when_right_matches() {
    // R has customer->3 orders; semijoin against a key-set must keep weight 3,
    // not corrupt it (§6).
    let r = rel(&[(1, 2, 3)]);
    let s = rel(&[(2, 99, 1)]);
    assert_eq!(algebra::semijoin(&r, &s).to_sorted_vec(), vec![(int(1), int(2), 3)]);
    // No match -> dropped.
    let s2 = rel(&[(7, 99, 1)]);
    assert!(algebra::semijoin(&r, &s2).is_empty());
}

#[test]
fn union_adds_and_intersect_mins() {
    let r = rel(&[(1, 2, 1), (1, 3, 2)]);
    let s = rel(&[(1, 2, 1)]);
    assert_eq!(algebra::union(&r, &s).weight(&int(1), &int(2)), 2);
    assert_eq!(algebra::intersect(&r, &s).to_sorted_vec(), vec![(int(1), int(2), 1)]);
}

#[test]
fn distinct_clamps_to_one() {
    let r = rel(&[(1, 2, 5), (1, 3, -1)]);
    assert_eq!(algebra::distinct(&r).to_sorted_vec(), vec![(int(1), int(2), 1)]);
}

#[test]
fn fork_tuples_the_right_columns() {
    let r = rel(&[(1, 10, 1)]);
    let s = rel(&[(1, 20, 1)]);
    let out = algebra::fork(&r, &s);
    let pair = Value::Pair(Box::new(int(10)), Box::new(int(20)));
    assert_eq!(out.to_sorted_vec(), vec![(int(1), pair, 1)]);
}

#[test]
fn value_join_combines_co_keyed_values() {
    let qty = rel(&[(1, 3, 1)]);
    let price = rel(&[(1, 100, 1)]);
    let out = algebra::value_join(&qty, &price, |a, b| {
        Value::Int(a.as_i64().unwrap() * b.as_i64().unwrap())
    });
    assert_eq!(out.to_sorted_vec(), vec![(int(1), int(300), 1)]);
}

#[test]
fn aggregate_sum_respects_weight() {
    // key 1 has values 10 (x2) and 5 -> sum 25; key 2 has 7 -> 7.
    let image = rel(&[(1, 10, 2), (1, 5, 1), (2, 7, 1)]);
    let out = algebra::aggregate(&image, algebra::Agg::Sum, false);
    assert_eq!(out.weight(&int(1), &int(25)), 1);
    assert_eq!(out.weight(&int(2), &int(7)), 1);
}

// --- §12 end-to-end -------------------------------------------------------

/// Collect a view as a map from left key -> right value (weight-1 rows).
fn view_map(result: &rex::eval::EvalResult, name: &str) -> BTreeMap<Value, Value> {
    result
        .view(name)
        .unwrap_or_else(|| panic!("no view `{name}`"))
        .iter()
        .map(|(l, r, _)| (l, r))
        .collect()
}

#[test]
fn spec12_runs_end_to_end() {
    let src = include_str!("fixtures/spec12.rex");
    let parsed = parse(src);
    assert!(parsed.diagnostics.is_empty());
    let result = run(&parsed.program);

    // Customer sort is minted first (index 0); alice=#0:0, bob=#0:1.
    let cust = |n| Value::Id(rex::types::SortId(0), n);

    // lineprice: 4 lines. qty * price(cents): 3*999, 1*2450, 2*999, 5*2450.
    let lineprice: Vec<Value> = {
        let mut v: Vec<_> = result
            .view("lineprice")
            .unwrap()
            .iter()
            .map(|(_, r, _)| r)
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        lineprice,
        vec![
            Value::Money(1998),
            Value::Money(2450),
            Value::Money(2997),
            Value::Money(12250),
        ]
    );

    // custspend: alice = 2997+2450+1998 = 7445; bob = 12250.
    let custspend = view_map(&result, "custspend");
    assert_eq!(custspend[&cust(0)], Value::Money(7445));
    assert_eq!(custspend[&cust(1)], Value::Money(12250));

    // result: West/East customers with spend > $30 -> both, with their totals.
    let out = view_map(&result, "result");
    assert_eq!(out.len(), 2);
    assert_eq!(out[&cust(0)], Value::Money(7445));
    assert_eq!(out[&cust(1)], Value::Money(12250));
}

// --- recursion (§8) ---------------------------------------------------------

/// A four-node graph program: `a`..`d`, the given edges, an `edge` view, and
/// `views` appended (typically recursive lets).
fn graph_src(edges: &[(&str, &str)], views: &str) -> String {
    let mut s = String::from(
        "entity Node { name: Text }\nentity Edge { src: NodeID, dst: NodeID }\n",
    );
    for n in ["a", "b", "c", "d"] {
        s.push_str(&format!("let {n} = new Node {{ name: \"{n}\" }}\n"));
    }
    for (x, y) in edges {
        s.push_str(&format!("let _ = new Edge {{ src: {x}, dst: {y} }}\n"));
    }
    s.push_str(
        "let srcof : Edge -> Node = :src\n\
         let dstof : Edge -> Node = :dst\n\
         let edge : Node -> Node = dstof by srcof\n",
    );
    s.push_str(views);
    s
}

/// A view's pairs as node indices (`a`=0 .. `d`=3), sorted, asserting every
/// weight is 1 (set semantics at the knot).
fn node_pairs(result: &rex::eval::EvalResult, name: &str) -> Vec<(u64, u64)> {
    let node = |v: &Value| match v {
        Value::Id(_, n) => *n,
        other => panic!("expected a node id, got {other}"),
    };
    let mut out: Vec<(u64, u64)> = result
        .view(name)
        .unwrap_or_else(|| panic!("no view `{name}`"))
        .iter()
        .map(|(l, r, w)| {
            assert_eq!(w, 1, "fixpoint views are sets");
            (node(&l), node(&r))
        })
        .collect();
    out.sort();
    out
}

fn run_src(src: &str) -> rex::eval::EvalResult {
    let parsed = parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    run(&parsed.program)
}

#[test]
fn transitive_closure_of_a_chain() {
    let src = graph_src(
        &[("a", "b"), ("b", "c"), ("c", "d")],
        "let recursive path : Node -> Node = edge + edge . path\n",
    );
    assert_eq!(
        node_pairs(&run_src(&src), "path"),
        vec![(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)]
    );
}

#[test]
fn closure_terminates_on_a_cycle() {
    // a -> b -> c -> a: every node reaches every node (incl. itself); the
    // forced distinct at the knot is what stops the iteration.
    let src = graph_src(
        &[("a", "b"), ("b", "c"), ("c", "a")],
        "let recursive path : Node -> Node = edge + edge . path\n",
    );
    let all: Vec<(u64, u64)> =
        (0..3).flat_map(|l| (0..3).map(move |r| (l, r))).collect();
    assert_eq!(node_pairs(&run_src(&src), "path"), all);
}

#[test]
fn mutual_recursion_odd_even_paths() {
    // odd = paths of odd length, even = paths of even (>= 2) length.
    let src = graph_src(
        &[("a", "b"), ("b", "c"), ("c", "d")],
        "let recursive odd : Node -> Node = edge + edge . even\n\
         let recursive even : Node -> Node = edge . odd\n",
    );
    let result = run_src(&src);
    assert_eq!(
        node_pairs(&result, "odd"),
        vec![(0, 1), (0, 3), (1, 2), (2, 3)]
    );
    assert_eq!(node_pairs(&result, "even"), vec![(0, 2), (1, 3)]);
}

#[test]
fn recursion_fixture_runs_end_to_end() {
    let result = run_src(include_str!("fixtures/recursion.rex"));
    assert_eq!(
        node_pairs(&result, "path"),
        vec![(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)]
    );
}

#[test]
fn empty_edge_relation_yields_empty_closure() {
    let src = graph_src(&[], "let recursive path : Node -> Node = edge + edge . path\n");
    assert_eq!(node_pairs(&run_src(&src), "path"), vec![]);
}
