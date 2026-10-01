//! Random event histories over the real example programs, with hostile
//! arguments (ids that never existed, `i64::MAX`, text full of the wire
//! encoding's own punctuation, relation args with zero and negative weights).
//!
//! After every dispatch: the base invariant holds, every view equals batch
//! evaluation over the same base tables, and the step's deltas are exactly the
//! change. At the end: the log replays to the same state (silently and not),
//! and a snapshot taken mid-history plus the rest of the log does too.

mod common;

use common::{check_history, Op};
use proptest::prelude::*;

fn example(path: &str) -> String {
    std::fs::read_to_string(format!("{}/../../examples/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn op() -> impl Strategy<Value = Op> {
    (any::<usize>(), prop::collection::vec(any::<usize>(), 8)).prop_map(|(event, picks)| Op { event, picks })
}

fn history() -> impl Strategy<Value = (Vec<Op>, usize)> {
    prop::collection::vec(op(), 1..24).prop_flat_map(|ops| {
        let n = ops.len();
        (Just(ops), 0..=n)
    })
}

/// A program that leans on what the examples don't: an entity that refers to
/// itself, a link entity, a defaultless state, `do` chains, handlers that
/// create and destroy in one transaction, arithmetic on big values, and
/// recursion over data that events change.
const GRAPH: &str = r#"
type Colour = Red | Green | Blue
entity Node { name: Text, weight: Int, colour: Colour, cost: Money }
// A key: at most one edge between a pair, so a second `Link` is rejected.
entity Edge { src: Node, dst: Node, key (src, dst) }
state root : Node
state total : Int = 0

event AddNode(name: Text, weight: Int, colour: Colour, cost: Money)
event Link(a: Node, b: Node)
event Unlink(a: Node)
event Flip(a: Node, b: Node)
event Reweigh(n: Node, d: Int)
event Paint(c: Colour)
event Drop(n: Node)
event DropHeavy(limit: Int)
event Churn(name: Text)
event SetRoot(n: Node)
event Both(n: Node, d: Int)
event Rename(n: Node, name: Text)

on AddNode(name, weight, colour, cost) {
  new Node { name: name, weight: weight, colour: colour, cost: cost }
  set total = total + weight
}
on Link(a, b)       => new Edge { src: a, dst: b }
on Unlink(a)        => delete Edge where .src = a
// A row test over two params, and a key given up and retaken in one event.
on Flip(a, b) {
  if (Edge where .src = a & .dst = b) {
    delete Edge where .src = a & .dst = b
    new Edge { src: a, dst: b }
  } else if (not (Edge where .src = b & .dst = a)) {
    new Edge { src: b, dst: a }
  }
}
// A guard: a keyset view (`n` exists and is not too heavy) and an evaluated
// conjunct (the argument alone).
on Reweigh(n, d) where (n.weight < 1000 & d != 0) else "nothing to do" => n.weight := n.weight + d
// A branch on an argument; one arm is a bulk update, the other a state write.
on Paint(c) {
  if (c = Red) { update Node where .weight > 3 { colour: c } }
  else if (heavy_n > 1) { update Node where .weight > 3 { colour: c }; set total = total + 1 }
  else { set total = total - 1 }
  // A reject after the writes above: an argument and a view, in one condition.
  if (c = Blue & heavy_n > 2) { reject "too blue" }
}
on Drop(n)          => delete n
on DropHeavy(limit) => delete Node where .weight > limit
on SetRoot(n)       => set root = n
on Rename(n, name)  => n.name := n.name ++ name
// Writes that compose within one transaction: set twice, then delete.
on Churn(name) {
  update Node where .colour = Red { name: name }
  update Node where .colour = Red { name: .name ++ "!" }
  delete Node where .weight < 0
}
// A `do` chain: both callees join the outer transaction.
on Both(n, d) {
  do Reweigh(n, d)
  do Rename(n, "+")
}

let heavy_n : Unit -> Int = count((Node where .weight > 3) by unit)

let n0 = new Node { name: "a", weight: 1, colour: Red,   cost: 1.50 }
let n1 = new Node { name: "b", weight: 5, colour: Green, cost: 0.25 }
let n2 = new Node { name: "c", weight: 9, colour: Blue,  cost: 10.00 }
let _  = new Edge { src: n0, dst: n1 }
let _  = new Edge { src: n1, dst: n2 }

let srcof : Edge -> Node = .src
let dstof : Edge -> Node = .dst
let edge  : Node -> Node = dstof by srcof
let recursive reach : Node -> Node = edge | edge . reach

let heavy      : Node = Node where .weight > 3
let light      : Node = Node where not .weight > 3
let by_colour  : Colour -> Int = count(Node by .colour)
let weight_of  : Node -> Int = .weight
let cost_of    : Node -> Money = .cost
let weight_sum : Unit -> Int = sum(weight_of by unit)
let cost_sum   : Colour -> Money = sum(cost_of by .colour)
let lightest   : Colour -> Int = min(weight_of by .colour)
let heaviest   : Colour -> Int = max(weight_of by .colour)
let mean_w     : Colour -> Money = avg(weight_of by .colour)
let mean_cost  : Unit -> Money = avg(cost_of by unit)
let out_degree : Node -> Int = count(Edge by .src)
let dangling   : Edge -> Edge = Edge[(.dst) except Node]
let reach_n    : Node -> Int = count(~reach by id)
let is_root    : Node = Node where id = root
let labelled   : Node -> Text = .name ++ ":" ++ (if .weight > 3 then "heavy" else "light")
let scaled     : Node -> Money = .cost * .weight
let pairs      : Node -> Int * Text = (.weight , .name)
let named      : Node -> Text = snd pairs
let shade      : Node -> Text = match .colour { Red => "r", Green => "g", _ => "other" }
let total_view : Unit -> Int = total
// Views that name seed rows: the names must mean the same ids on a
// restoring boot, which skips the `new`s that bind them.
let from_a     : Edge = Edge[(.src) . n0]
let to_c       : Edge -> Node = .dst . n2
let a_name     : Node -> Text = n0 . .name
"#;

proptest! {
    #![proptest_config(ProptestConfig { cases: common::cases(96), ..ProptestConfig::default() })]

    #[test]
    fn todomvc((ops, cut) in history()) {
        check_history(&example("todomvc/src/app.rex"), &ops, cut).map_err(TestCaseError::fail)?;
    }

    #[test]
    fn benchmark((ops, cut) in history()) {
        check_history(&example("js-framework-benchmark/src/app.rex"), &ops, cut).map_err(TestCaseError::fail)?;
    }

    #[test]
    fn kanban((ops, cut) in history()) {
        check_history(&example("kanban/src/board.rex"), &ops, cut).map_err(TestCaseError::fail)?;
    }

    #[test]
    fn chat((ops, cut) in history()) {
        check_history(&example("chat/src/app.rex"), &ops, cut).map_err(TestCaseError::fail)?;
    }

    #[test]
    fn graph((ops, cut) in history()) {
        check_history(GRAPH, &ops, cut).map_err(TestCaseError::fail)?;
    }
}
