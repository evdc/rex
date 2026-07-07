//! M2 gate: composite-key nesting (nesting-draft §1) over the live engine.
//!
//! Nesting = flat relations keyed by the composite of enclosing grouping keys,
//! built entirely from existing combinators: `(:list , id)` forks the
//! membership key with the entity id, `~keyed . attr` produces the per-level
//! indexed relation `(ListID × CardID) -> attr`, and `fst`/`snd` project the
//! key components back out. The assertions here are the shaper's input
//! contract: each kind of edit shows up as the right per-view delta —
//! reparent as `−/+` on the membership view at the same card, retitle as
//! `−/+` on the attribute view, never a whole-entity remove-plus-mount.

use rex::dbsp::Engine;
use rex::eval::relation::{BTreeRelation, BinaryRelation};
use rex::eval::Value;
use rex::types::typed::TProgram;
use std::collections::HashMap;

const FIXTURE: &str = "\
entity List { title: Text }
entity Card { title: Text, pos: Text, list: ListID }

let l_todo  = new List { title: \"Todo\" }
let l_doing = new List { title: \"Doing\" }
let c1 = new Card { title: \"Buy milk\", pos: \"a0\", list: l_todo }
let c2 = new Card { title: \"Ship it\",  pos: \"a1\", list: l_todo }

let card_list  : Card -> ListID = :list
let card_title : Card -> Text   = :title
let card_pos   : Card -> Text   = :pos
let keyed      : Card -> ListID * CardID = (:list , id)
let titled     = ~keyed . card_title
let key_list   : Card -> ListID = fst keyed
";

const VIEWS: [&str; 5] = ["card_list", "card_title", "card_pos", "titled", "key_list"];

fn elaborate(src: &str) -> TProgram {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "check: {:?}", checked.diagnostics);
    checked.elaborated.expect("clean check produces the elaborated program")
}

/// Apply the whole fixture, returning the engine and the `new`-bound ids.
fn build() -> (Engine, HashMap<String, Value>) {
    let prog = elaborate(FIXTURE);
    let mut engine = Engine::new();
    let mut values: HashMap<String, Value> = HashMap::new();
    for stmt in &prog.stmts {
        engine.apply_typed_stmt(stmt, &mut values);
    }
    (engine, values)
}

fn pair(a: &Value, b: &Value) -> Value {
    Value::Pair(Box::new(a.clone()), Box::new(b.clone()))
}

fn text(s: &str) -> Value {
    Value::text(s)
}

/// The expected delta as sorted (left, right, weight) triples.
fn triples(rel: &BTreeRelation) -> Vec<(Value, Value, i64)> {
    rel.iter().collect()
}

fn delta<'a>(res: &'a rex::dbsp::StepResult, view: &str) -> &'a BTreeRelation {
    res.view_deltas.get(view).unwrap_or_else(|| panic!("no delta for view `{view}`"))
}

#[test]
fn backfill_materializes_composite_keys() {
    let (engine, values) = build();
    let (todo, c1, c2) = (&values["l_todo"], &values["c1"], &values["c2"]);

    // The composite-keyed view is the per-level IndexedZSet: one row per
    // (list, card) pair, valued by the card's title.
    let titled = engine.circuit.view("titled").expect("view");
    assert_eq!(
        triples(titled),
        vec![
            (pair(todo, c1), text("Buy milk"), 1),
            (pair(todo, c2), text("Ship it"), 1),
        ]
    );

    // fst recovers the membership relation from the forked key.
    assert_eq!(
        engine.circuit.view("key_list").expect("view"),
        engine.circuit.view("card_list").expect("view"),
    );
}

#[test]
fn insert_emits_positive_deltas_on_every_level() {
    let (mut engine, values) = build();
    let doing = values["l_doing"].clone();

    let (c3, res) = engine.apply_new(
        rex::types::ty::SortId(1), // Card is the second entity declared
        &[
            ("title".into(), text("New card")),
            ("pos".into(), text("a2")),
            ("list".into(), doing.clone()),
        ],
    );

    assert_eq!(triples(delta(&res, "card_list")), vec![(c3.clone(), doing.clone(), 1)]);
    assert_eq!(triples(delta(&res, "card_title")), vec![(c3.clone(), text("New card"), 1)]);
    assert_eq!(triples(delta(&res, "titled")), vec![(pair(&doing, &c3), text("New card"), 1)]);
}

#[test]
fn retitle_is_same_key_retract_assert_on_attr_view_only() {
    let (mut engine, values) = build();
    let (todo, c1) = (values["l_todo"].clone(), values["c1"].clone());

    let res = engine.update_field(&c1, "title", text("Buy oat milk"));

    // Attribute view: −old/+new at the same card key — the fusion signal.
    assert_eq!(
        triples(delta(&res, "card_title")),
        vec![
            (c1.clone(), text("Buy milk"), -1),
            (c1.clone(), text("Buy oat milk"), 1),
        ]
    );
    // Composite level sees the same shape at the composite key.
    assert_eq!(
        triples(delta(&res, "titled")),
        vec![
            (pair(&todo, &c1), text("Buy milk"), -1),
            (pair(&todo, &c1), text("Buy oat milk"), 1),
        ]
    );
    // Structural and order views are untouched: a retitle is never a move.
    assert!(delta(&res, "card_list").is_empty());
    assert!(delta(&res, "card_pos").is_empty());
}

#[test]
fn regroup_is_membership_flip_and_composite_rekey() {
    let (mut engine, values) = build();
    let (todo, doing, c1) =
        (values["l_todo"].clone(), values["l_doing"].clone(), values["c1"].clone());

    // Kanban drag: c1 moves Todo -> Doing.
    let res = engine.update_field(&c1, "list", doing.clone());

    // Membership view: −/+ at the same card — the shaper classifies this as
    // `reparent` (reuse the DOM node), never remove+mount.
    let mut expected = vec![(c1.clone(), doing.clone(), 1), (c1.clone(), todo.clone(), -1)];
    expected.sort();
    assert_eq!(triples(delta(&res, "card_list")), expected);
    // Composite level: the row moves to the new composite key, same value.
    let mut expected = vec![
        (pair(&todo, &c1), text("Buy milk"), -1),
        (pair(&doing, &c1), text("Buy milk"), 1),
    ];
    expected.sort();
    assert_eq!(triples(delta(&res, "titled")), expected);
    // The attribute view is untouched: a move is never a field update.
    assert!(delta(&res, "card_title").is_empty());
}

#[test]
fn retract_cascades_negative_deltas_through_every_level() {
    let (mut engine, values) = build();
    let (todo, c2) = (values["l_todo"].clone(), values["c2"].clone());

    let res = engine.retract_entity(&c2);

    assert_eq!(triples(delta(&res, "card_list")), vec![(c2.clone(), todo.clone(), -1)]);
    assert_eq!(triples(delta(&res, "card_title")), vec![(c2.clone(), text("Ship it"), -1)]);
    assert_eq!(triples(delta(&res, "titled")), vec![(pair(&todo, &c2), text("Ship it"), -1)]);

    // And the integrated composite view holds only the surviving card.
    let titled = engine.circuit.view("titled").expect("view");
    assert_eq!(titled.len(), 1);
}

#[test]
fn update_fields_moves_pos_and_list_in_one_atomic_step() {
    let (mut engine, values) = build();
    let (todo, doing, c1) =
        (values["l_todo"].clone(), values["l_doing"].clone(), values["c1"].clone());

    // A Kanban drag: reparent Todo -> Doing and reposition, in one transaction.
    let res = engine.update_fields(
        &c1,
        &[("list".into(), doing.clone()), ("pos".into(), text("b5"))],
    );

    // Both changes surface in the same batch — the membership flip and the
    // order-key change are one consistent StepResult, never two.
    let mut mem = vec![(c1.clone(), doing.clone(), 1), (c1.clone(), todo.clone(), -1)];
    mem.sort();
    assert_eq!(triples(delta(&res, "card_list")), mem);
    assert_eq!(
        triples(delta(&res, "card_pos")),
        vec![(c1.clone(), text("a0"), -1), (c1.clone(), text("b5"), 1)]
    );
}

#[test]
fn update_after_retract_is_a_no_op_never_an_orphan() {
    let (mut engine, values) = build();
    let c2 = values["c2"].clone();

    engine.retract_entity(&c2);
    // A stale UI event arriving after the card is gone must not resurrect it
    // as a title row with no backing identity.
    let res = engine.update_field(&c2, "title", text("Zombie"));

    assert!(res.view_deltas.values().all(|d| d.is_empty()), "stale update must be inert");
    let titled = engine.circuit.view("card_title").expect("view");
    assert!(titled.row(&c2).next().is_none(), "no orphaned field row for a dead entity");
}

/// Every scenario, replayed, must leave the integrals equal to a from-scratch
/// batch evaluation of the equivalent final program (the oracle property).
#[test]
fn integrals_match_batch_after_edit_storm() {
    let (mut engine, values) = build();
    let (doing, c1) = (values["l_doing"].clone(), values["c1"].clone());

    engine.update_field(&c1, "title", text("Buy oat milk"));
    engine.update_field(&c1, "list", doing.clone());
    engine.retract_entity(&values["c2"]);

    // Batch oracle: the same final state written directly.
    let batch_src = "\
entity List { title: Text }
entity Card { title: Text, pos: Text, list: ListID }
let l_todo  = new List { title: \"Todo\" }
let l_doing = new List { title: \"Doing\" }
let c1 = new Card { title: \"Buy oat milk\", pos: \"a0\", list: l_doing }

let card_list  : Card -> ListID = :list
let card_title : Card -> Text   = :title
let card_pos   : Card -> Text   = :pos
let keyed      : Card -> ListID * CardID = (:list , id)
let titled     = ~keyed . card_title
let key_list   : Card -> ListID = fst keyed
";
    let batch = rex::eval::run(&rex::parse(batch_src).program);
    for name in VIEWS {
        let incremental = engine.circuit.view(name).expect("view");
        let oracle = batch.view(name).expect("batch view");
        // Direct equality holds because both runs mint the same ids: List
        // 0/1, and c1 is the first Card in each.
        assert_eq!(incremental, oracle, "view `{name}` diverged from batch");
    }
}
