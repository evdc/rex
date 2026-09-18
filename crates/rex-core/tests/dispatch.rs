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

let l0 = new List { title: "Todo",  pos: "a0" }
let l1 = new List { title: "Doing", pos: "a1" }
let c0 = new Card { title: "Design", pos: "a0", list: l0 }

let card_list  : Card -> ListID = .list
let card_pos   : Card -> Text = .pos

event MoveCard(card: Card, list: List, pos: Text)
event AddList(title: Text, pos: Text)
event AddCardAndMove(title: Text, list: List, card: Card)
event Rename(card: Card, title: Text)
event RenameTwice(card: Card)
event Nope(card: Card)

on MoveCard(card, list, pos) => update card { list: list, pos: pos }
on AddList(title, pos)       => new List { title: title, pos: pos }
on AddCardAndMove(title, list, card) {
  new Card { title: title, pos: "a2", list: list }
  do MoveCard(card, list, "a3")
}
on Rename(card, title) => card.title := title
on RenameTwice(card) { do Rename(card, "first"); do Rename(card, "second") }
on Nope(card) => card.title := "never"
"#;

struct App {
    engine: Engine,
    env: Env,
    events: Vec<EventDef>,
    values: HashMap<String, Value>,
}

impl App {
    fn dispatch(&mut self, name: &str, args: &[(&str, Value)]) -> Result<(Vec<Value>, rex::dbsp::StepResult), String> {
        let args: HashMap<String, Value> = args.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        dispatch_event(&mut self.engine, &self.env, &self.events, name, &args)
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
