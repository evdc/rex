//! S-20: a named `event` dispatches as ONE atomic transaction (one step, one
//! delta batch), with every read against the pre-event snapshot, through the
//! shared [`rex::events::dispatch_event`] path the wasm bridge also uses.

use rex::dbsp::Engine;
use rex::eval::relation::BinaryRelation;
use rex::eval::Value;
use rex::events::dispatch_event;
use rex::types::shape_ir::EventDef;
use rex::types::typed::TProgram;
use rex::types::Env;
use std::collections::HashMap;

const SRC: &str = r#"
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: ListID }
entity Task { n: Int, done: {@yes | @no}, big: Bool }

let l0 = new List { title: "Todo",  pos: "a0" }
let l1 = new List { title: "Doing", pos: "a1" }
let c0 = new Card { title: "Design", pos: "a0", list: l0 }
let t0 = new Task { n: 5, done: @no, big: False }

let card_list  : Card -> ListID = .list
let card_pos   : Card -> Text = .pos
let task_n     : Task -> Int = .n
let task_done  : Task -> {@yes | @no} = .done
let task_big   : Task -> Bool = .big

entity Row { label: Text, pos: Int }
let r0 = new Row { label: "r0", pos: 0 }
let r1 = new Row { label: "r1", pos: 1 }
let r2 = new Row { label: "r2", pos: 2 }
let r3 = new Row { label: "r3", pos: 3 }
let row_label : Row -> Text = .label
let row_pos   : Row -> Int = .pos

event MoveCard(card: Card, list: List, pos: Text)
event AddList(title: Text, pos: Text)
event AddCardAndMove(title: Text, list: List, card: Card)
event Rename(card: Card, title: Text)
event RenameTwice(card: Card)
event Nope(card: Card)
event ToggleTask(t: Task)
event IncrementTask(t: Task, amount: Int)
event MarkBig(t: Task, limit: Int)
event ClearAllRows()
event RelabelAllRows(label: Text)
event DeleteHighPos()
event RelabelAtPos(pos: Int)
event MakeRows(rows: Int -> Text)

on MoveCard(card, list, pos) => update card { list: list, pos: pos }
on AddList(title, pos)       => new List { title: title, pos: pos }
on AddCardAndMove(title, list, card) {
  new Card { title: title, pos: "a2", list: list }
  do MoveCard(card, list, "a3")
}
on Rename(card, title) => card.title := title
on RenameTwice(card) { do Rename(card, "first"); do Rename(card, "second") }
on Nope(card) => card.title := "never"
on ToggleTask(t)        => t.done := not t.done
on IncrementTask(t, amount) => t.n := t.n + amount
on MarkBig(t, limit)    => t.big := t.n > limit
on RelabelAllRows(label) => update Row { label: label }
on ClearAllRows()       => delete Row
on DeleteHighPos()      => delete Row where .pos > 1
on RelabelAtPos(pos)    => update Row where .pos = pos { label: "hit" }
on MakeRows(rows)       => new Row from rows as (k, v) { pos: k, label: v }
"#;

struct App {
    engine: Engine,
    env: Env,
    events: Vec<EventDef>,
    values: HashMap<String, Value>,
}

impl App {
    fn dispatch(&mut self, name: &str, args: &[(&str, Value)]) -> Result<(Vec<Value>, rex::dbsp::StepResult), String> {
        let args: HashMap<String, rex::dbsp::ArgValue> =
            args.iter().map(|(k, v)| (k.to_string(), rex::dbsp::ArgValue::Value(v.clone()))).collect();
        dispatch_event(&mut self.engine, &self.env, &self.events, name, &args).map_err(|e| e.to_string())
    }

    /// Like [`dispatch`](Self::dispatch), but for an event with a
    /// relation-typed param (S-42): `rel` is `(key, value)` rows.
    fn dispatch_rel(
        &mut self,
        name: &str,
        scalars: &[(&str, Value)],
        rel_param: &str,
        rel: &[(Value, Value)],
    ) -> Result<(Vec<Value>, rex::dbsp::StepResult), String> {
        let mut args: HashMap<String, rex::dbsp::ArgValue> =
            scalars.iter().map(|(k, v)| (k.to_string(), rex::dbsp::ArgValue::Value(v.clone()))).collect();
        args.insert(
            rel_param.to_string(),
            rex::dbsp::ArgValue::Rel(rel.iter().map(|(k, v)| (k.clone(), v.clone(), 1)).collect()),
        );
        dispatch_event(&mut self.engine, &self.env, &self.events, name, &args).map_err(|e| e.to_string())
    }
}

fn setup() -> App {
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
    App { engine, env: checked.env, events: checked.shapes.events, values }
}

fn text(s: &str) -> Value {
    Value::Text(rex::eval::intern(s))
}

/// The live (positive-weight) value of `id`'s `field`, read straight from
/// the engine's current input integral — the same shape S-40's point
/// evaluator reads at dispatch time.
fn read_field(app: &App, id: &Value, field: &str) -> Value {
    let Value::Id(sort, _) = id else { unreachable!() };
    app.engine
        .circuit
        .input_integral(&rex::dbsp::InputKey::Field(*sort, rex::eval::intern(field)))
        .unwrap()
        .row(id)
        .find(|(_, w)| *w > 0)
        .map(|(v, _)| v)
        .unwrap_or_else(|| panic!("no live `{field}` value for {id:?}"))
}

#[test]
fn move_is_one_atomic_reparent() {
    // A Kanban drag sets both `list` and `pos` of a card. As one event it is a
    // single step: the membership view (`card_list`) shows exactly the
    // −old/+new pair for the moved card, never a torn intermediate.
    let mut app = setup();
    let c0 = app.values["c0"].clone();
    let l0 = app.values["l0"].clone();
    let l1 = app.values["l1"].clone();

    let (ids, res) = app
        .dispatch("MoveCard", &[("card", c0.clone()), ("list", l1.clone()), ("pos", text("a5"))])
        .unwrap();
    assert!(ids.is_empty(), "no ids minted by a pure set");

    let membership = res.view_deltas.get("card_list").expect("card_list delta");
    assert_eq!(membership.weight(&c0, &l0), -1);
    assert_eq!(membership.weight(&c0, &l1), 1);

    assert_eq!(app.engine.circuit.view("card_list").unwrap().weight(&c0, &l1), 1);
    assert_eq!(app.engine.circuit.view("card_list").unwrap().weight(&c0, &l0), 0);
}

#[test]
fn new_returns_minted_id() {
    let mut app = setup();
    let (ids, _res) = app
        .dispatch("AddList", &[("title", text("New list")), ("pos", text("a9"))])
        .unwrap();
    assert_eq!(ids.len(), 1, "one id minted");
}

#[test]
fn synchronous_do_joins_the_transaction() {
    // `AddCardAndMove` creates a card and `do`es `MoveCard` for another: both
    // land in a single delta batch (one step), and only the outer event is
    // what the host dispatched.
    let mut app = setup();
    let c0 = app.values["c0"].clone();
    let l0 = app.values["l0"].clone();
    let l1 = app.values["l1"].clone();
    let (ids, res) = app
        .dispatch(
            "AddCardAndMove",
            &[("title", text("Fresh")), ("list", l1.clone()), ("card", c0.clone())],
        )
        .unwrap();
    assert_eq!(ids.len(), 1);
    let new_id = ids[0].clone();
    let m = res.view_deltas.get("card_list").expect("card_list delta");
    assert_eq!(m.weight(&new_id, &l1), 1);
    assert_eq!(m.weight(&c0, &l0), -1);
    assert_eq!(m.weight(&c0, &l1), 1);
    let pos = res.view_deltas.get("card_pos").expect("card_pos delta");
    assert_eq!(pos.weight(&c0, &text("a3")), 1);
}

#[test]
fn nested_do_reads_the_pre_event_snapshot() {
    // Two `do Rename`s in one event both negate the ORIGINAL title (the
    // pre-event snapshot), so the integrated result is one live row — the
    // last write — and never a double-retracted or duplicated title.
    let mut app = setup();
    let c0 = app.values["c0"].clone();
    let (_, res) = app.dispatch("RenameTwice", &[("card", c0.clone())]).unwrap();
    let _ = res;
    let titles: Vec<(Value, i64)> = app
        .engine
        .circuit
        .input_integral(&rex::dbsp::InputKey::Field(
            match &c0 {
                Value::Id(s, _) => *s,
                _ => unreachable!(),
            },
            rex::eval::intern("title"),
        ))
        .unwrap()
        .row(&c0)
        .collect();
    let live: Vec<_> = titles.iter().filter(|(_, w)| *w != 0).collect();
    assert_eq!(live.len(), 1, "exactly one live title: {titles:?}");
}

#[test]
fn dispatch_rejects_bad_args() {
    let mut app = setup();
    let c0 = app.values["c0"].clone();
    let l1 = app.values["l1"].clone();
    // Unknown event.
    assert!(app.dispatch("Vanish", &[]).unwrap_err().contains("unknown event"));
    // Missing arg.
    assert!(app.dispatch("Nope", &[]).unwrap_err().contains("missing arg `card`"));
    // Wrong entity for an id param.
    assert!(app.dispatch("Nope", &[("card", l1.clone())]).unwrap_err().contains("wrong type"));
    // Scalar of the wrong kind.
    assert!(app
        .dispatch("MoveCard", &[("card", c0.clone()), ("list", l1.clone()), ("pos", Value::Int(1))])
        .unwrap_err()
        .contains("wrong type"));
}

// --- S-40: mutation values as expressions over the pre-event snapshot -----

#[test]
fn toggle_flips_a_two_atom_field() {
    let mut app = setup();
    let t0 = app.values["t0"].clone();
    assert_eq!(read_field(&app, &t0, "done"), Value::atom("no"));

    app.dispatch("ToggleTask", &[("t", t0.clone())]).unwrap();
    assert_eq!(read_field(&app, &t0, "done"), Value::atom("yes"));

    // `not` reads the field fresh each time (the pre-event snapshot), so a
    // second toggle flips it back rather than getting stuck.
    app.dispatch("ToggleTask", &[("t", t0.clone())]).unwrap();
    assert_eq!(read_field(&app, &t0, "done"), Value::atom("no"));
}

#[test]
fn increment_adds_to_the_fields_current_value() {
    let mut app = setup();
    let t0 = app.values["t0"].clone();
    assert_eq!(read_field(&app, &t0, "n"), Value::Int(5));

    app.dispatch("IncrementTask", &[("t", t0.clone()), ("amount", Value::Int(3))]).unwrap();
    assert_eq!(read_field(&app, &t0, "n"), Value::Int(8));

    // Reads the (now-updated) pre-event snapshot again, not a stale copy.
    app.dispatch("IncrementTask", &[("t", t0.clone()), ("amount", Value::Int(-2))]).unwrap();
    assert_eq!(read_field(&app, &t0, "n"), Value::Int(6));
}

#[test]
fn conditional_set_from_a_comparison() {
    // `t.big := t.n > limit` computes a `Bool` field from a comparison. A
    // comparison's result type *is* `Bool` (S-50), so the field it lands in
    // has to be `Bool`-typed — the atoms are `@True`/`@False`, spelled the
    // way the constructors are.
    // against an event arg — the "conditional" mutation-value shape.
    let mut app = setup();
    let t0 = app.values["t0"].clone();
    assert_eq!(read_field(&app, &t0, "big"), Value::atom("False"));

    app.dispatch("MarkBig", &[("t", t0.clone()), ("limit", Value::Int(3))]).unwrap();
    assert_eq!(read_field(&app, &t0, "big"), Value::atom("True"), "5 > 3");

    app.dispatch("MarkBig", &[("t", t0.clone()), ("limit", Value::Int(10))]).unwrap();
    assert_eq!(read_field(&app, &t0, "big"), Value::atom("False"), "5 > 10 is false");
}

#[test]
fn increment_matches_an_independent_batch_oracle() {
    // The engine's point evaluator (`rex::events`'s `eval_val_expr`) and the
    // batch interpreter (`rex::eval::interp`) are two different evaluators
    // over the same program; they must agree (MVP-PLAN §2.10's "two
    // evaluators" risk). This computes the expected result via a *fresh*
    // batch interpretation of the source (never touching the live engine)
    // plus the shared `arith_values` kernel, then checks the live engine's
    // dispatch produced exactly that.
    let parsed = rex::parse(SRC);
    let batch = rex::eval::interp::run(&parsed.program);
    let before: Value = batch
        .view("task_n")
        .expect("task_n view")
        .iter()
        .next()
        .map(|(_, v, _)| v.clone())
        .expect("task_n has a row");
    let expected = rex::eval::interp::arith_values(rex::types::typed::ArithKind::Add, &before, &Value::Int(3), false);

    let mut app = setup();
    let t0 = app.values["t0"].clone();
    app.dispatch("IncrementTask", &[("t", t0.clone()), ("amount", Value::Int(3))]).unwrap();
    assert_eq!(read_field(&app, &t0, "n"), expected);
}

// --- S-41: `where`-targeted bulk update/delete as hidden views ------------

/// Whether `id` still has a live identity row (`None`, versus a stale
/// panic-on-missing-field read, is the right shape for asserting a bulk
/// delete actually retracted a row).
fn is_live(app: &App, id: &Value) -> bool {
    let Value::Id(sort, _) = id else { unreachable!() };
    app.engine
        .circuit
        .input_integral(&rex::dbsp::InputKey::Identity(*sort))
        .is_some_and(|rel| rel.row(id).any(|(_, w)| w > 0))
}

#[test]
fn delete_with_no_predicate_retracts_every_row() {
    // `delete Row` (S-41 subtask 3): the whole identity relation goes in one
    // transaction, not a scan that happens to match everything.
    let mut app = setup();
    let rows: Vec<Value> = ["r0", "r1", "r2", "r3"].iter().map(|n| app.values[*n].clone()).collect();
    for r in &rows {
        assert!(is_live(&app, r));
    }
    app.dispatch("ClearAllRows", &[]).unwrap();
    for r in &rows {
        assert!(!is_live(&app, r), "{r:?} should have been retracted");
    }
}

#[test]
fn update_with_no_predicate_sets_every_row() {
    // The counterpart of `delete Row`, and TodoMVC's `ToggleAll`: a bare
    // entity target means every row of it, in one transaction. S-41 landed
    // this for `delete` only, so `update Todo { … }` used to report "unknown
    // parameter `Todo` in mutation" — both verbs now share one target rule.
    let mut app = setup();
    let rows: Vec<Value> = ["r0", "r1", "r2", "r3"].iter().map(|n| app.values[*n].clone()).collect();
    let (_, step) = app.dispatch("RelabelAllRows", &[("label", text("same"))]).unwrap();
    for r in &rows {
        assert_eq!(read_field(&app, r, "label"), text("same"));
    }
    // One step, and every row's label moved in it: four retractions and four
    // assertions in a single delta batch, not four transactions.
    let delta = step.view_deltas.get("row_label").expect("row_label delta");
    assert_eq!(delta.triples().filter(|(_, _, w)| *w < 0).count(), 4);
    assert_eq!(delta.triples().filter(|(_, _, w)| *w > 0).count(), 4);
}

#[test]
fn where_delete_arg_free_uses_a_materialized_hidden_view() {
    // `delete Row where .pos > 1`: the predicate reads no event arg, so
    // `Desugar::mutation_target` desugars it to a hidden `let` (subtask 1) —
    // check that view exists and already holds exactly the matching keys
    // *before* any dispatch, proving dispatch reads a materialized keyset
    // rather than scanning the entity itself.
    let app = setup();
    let hidden = app.engine.circuit.view("on#DeleteHighPos#1").expect("hidden keyset view for the arg-free `where`");
    let r0 = app.values["r0"].clone();
    let r1 = app.values["r1"].clone();
    let r2 = app.values["r2"].clone();
    let r3 = app.values["r3"].clone();
    assert_eq!(hidden.weight(&r2, &r2), 1);
    assert_eq!(hidden.weight(&r3, &r3), 1);
    assert_eq!(hidden.weight(&r0, &r0), 0);
    assert_eq!(hidden.weight(&r1, &r1), 0);

    let mut app = app;
    app.dispatch("DeleteHighPos", &[]).unwrap();
    assert!(is_live(&app, &r0));
    assert!(is_live(&app, &r1));
    assert!(!is_live(&app, &r2));
    assert!(!is_live(&app, &r3));
}

#[test]
fn where_update_arg_dependent_scans_by_predicate() {
    // `update Row where .pos = pos { … }`: the predicate reads the event's
    // own arg, so it can't be a static view (subtask 2) — it's evaluated per
    // row at dispatch time instead. Exactly the one row whose `pos` matches
    // the dispatched arg gets updated, never its neighbors.
    let mut app = setup();
    let r0 = app.values["r0"].clone();
    let r1 = app.values["r1"].clone();
    let r2 = app.values["r2"].clone();

    app.dispatch("RelabelAtPos", &[("pos", Value::Int(1))]).unwrap();
    assert_eq!(read_field(&app, &r0, "label"), text("r0"));
    assert_eq!(read_field(&app, &r1, "label"), text("hit"));
    assert_eq!(read_field(&app, &r2, "label"), text("r2"));

    // Reads the pre-event snapshot fresh each dispatch, so a second arg
    // value hits a different row without disturbing the first.
    app.dispatch("RelabelAtPos", &[("pos", Value::Int(2))]).unwrap();
    assert_eq!(read_field(&app, &r1, "label"), text("hit"));
    assert_eq!(read_field(&app, &r2, "label"), text("hit"));
}

// --- S-42: `insert … from` with relation-valued params ---------------------

#[test]
fn new_from_mints_one_row_per_source_row_in_one_transaction() {
    // `new Row from rows as (k, v) { pos: k, label: v }`: a relation-typed
    // param (`rows: Int -> Text`) mints one entity per row, all in the one
    // transaction `dispatch_event` builds — never N separate `new`s.
    let mut app = setup();
    let (ids, res) = app
        .dispatch_rel(
            "MakeRows",
            &[],
            "rows",
            &[(Value::Int(10), text("ten")), (Value::Int(20), text("twenty")), (Value::Int(30), text("thirty"))],
        )
        .unwrap();
    assert_eq!(ids.len(), 3, "one id minted per source row");
    for (id, (k, v)) in ids.iter().zip([(10, "ten"), (20, "twenty"), (30, "thirty")]) {
        assert_eq!(read_field(&app, id, "pos"), Value::Int(k));
        assert_eq!(read_field(&app, id, "label"), text(v));
    }
    let membership = res.view_deltas.get("row_label").expect("row_label delta");
    assert_eq!(membership.iter().filter(|(_, _, w)| *w > 0).count(), 3, "one delta row per minted entity");
}

#[test]
fn new_from_mints_in_key_order() {
    // Subtask 2: "order of minting = key order" — ids come back sorted by
    // the source relation's key, regardless of the order rows were passed in.
    let mut app = setup();
    let (ids, _) = app
        .dispatch_rel("MakeRows", &[], "rows", &[(Value::Int(3), text("c")), (Value::Int(1), text("a")), (Value::Int(2), text("b"))])
        .unwrap();
    let keys: Vec<Value> = ids.iter().map(|id| read_field(&app, id, "pos")).collect();
    assert_eq!(keys, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
}
