//! M5.c: `Engine::dispatch` runs a batch of create/set/retract ops as ONE
//! atomic transaction (one step, one delta batch), with all reads against the
//! pre-step snapshot.

use rex::dbsp::{DispatchOp, Engine};
use rex::eval::relation::BinaryRelation;
use rex::eval::Value;
use rex::types::typed::TProgram;
use std::collections::HashMap;

const SRC: &str = r#"
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: ListID }

let l0 = new List { title: "Todo",  pos: "a0" }
let l1 = new List { title: "Doing", pos: "a1" }
let c0 = new Card { title: "Design", pos: "a0", list: l0 }

let card_list  : Card -> ListID = .list
let card_pos   : Card -> Text = .pos
"#;

fn setup() -> (Engine, HashMap<String, Value>) {
    let parsed = rex::parse(SRC);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let prog: TProgram = checked.elaborated.unwrap();
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &prog.stmts {
        engine.apply_typed_stmt(stmt, &mut values);
    }
    (engine, values)
}

fn text(s: &str) -> Value {
    Value::Text(rex::eval::intern(s))
}

#[test]
fn drag_is_one_atomic_reparent() {
    // A Kanban drag sets both `list` and `pos` of a card. As one dispatch it
    // is a single step: the membership view (`card_list`) shows exactly the
    // −old/+new pair for the moved card, never a torn intermediate.
    let (mut engine, values) = setup();
    let c0 = values["c0"].clone();
    let l1 = values["l1"].clone();

    let (ids, res) = engine.dispatch(&[DispatchOp::Set {
        id: c0.clone(),
        updates: vec![("list".into(), l1.clone()), ("pos".into(), text("a5"))],
    }]);
    assert!(ids.is_empty(), "no ids minted by a pure set");

    let membership = res.view_deltas.get("card_list").expect("card_list delta");
    // Exactly the −old-list / +new-list pair at c0.
    assert_eq!(membership.weight(&c0, &values["l0"]), -1);
    assert_eq!(membership.weight(&c0, &l1), 1);

    // Final integrated state: c0 now under l1.
    assert_eq!(engine.circuit.view("card_list").unwrap().weight(&c0, &l1), 1);
    assert_eq!(engine.circuit.view("card_list").unwrap().weight(&c0, &values["l0"]), 0);
}

#[test]
fn new_returns_minted_id() {
    let (mut engine, values) = setup();
    let list_sort = match &values["l0"] {
        Value::Id(s, _) => *s,
        _ => unreachable!(),
    };
    let (ids, _res) = engine.dispatch(&[DispatchOp::New {
        sort: list_sort,
        fields: vec![("title".into(), text("New list")), ("pos".into(), text("a9"))],
    }]);
    assert_eq!(ids.len(), 1, "one id minted");
}

#[test]
fn multi_op_is_one_step() {
    // A create plus a set on an existing card, one dispatch. Both land in a
    // single delta batch (one step): the new card's membership and the moved
    // card's membership both appear in the same `card_list` delta.
    let (mut engine, values) = setup();
    let c0 = values["c0"].clone();
    let l0 = values["l0"].clone();
    let l1 = values["l1"].clone();
    let card_sort = match &c0 {
        Value::Id(s, _) => *s,
        _ => unreachable!(),
    };
    let (ids, res) = engine.dispatch(&[
        DispatchOp::New {
            sort: card_sort,
            fields: vec![
                ("title".into(), text("Fresh")),
                ("pos".into(), text("a2")),
                ("list".into(), l1.clone()),
            ],
        },
        DispatchOp::Set {
            id: c0.clone(),
            updates: vec![("list".into(), l1.clone())],
        },
    ]);
    assert_eq!(ids.len(), 1);
    let new_id = ids[0].clone();
    let m = res.view_deltas.get("card_list").expect("card_list delta");
    // New card mounts under l1; c0 reparents from l0 to l1 — same batch.
    assert_eq!(m.weight(&new_id, &l1), 1);
    assert_eq!(m.weight(&c0, &l0), -1);
    assert_eq!(m.weight(&c0, &l1), 1);
}
