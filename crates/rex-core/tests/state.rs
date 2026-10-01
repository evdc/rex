//! S-51: `state` as a singleton relation.
//!
//! Every `state s : T [= d]` is a field of one hidden `State#` row, minted as
//! an ordinary top-level `new` so it is logged as a genesis event and replay
//! reproduces it. A bare `s` in an expression is the composite
//! `unit . ~(State# . unit) . .s` — every hop an operator the engine already
//! maintains, which is the whole point: changing state is *one field delta
//! flowing through two joins*, not a recompute of everything downstream
//! (MVP-PLAN §2.4).

use rex::dbsp::{ArgValue, Engine};
use rex::eval::relation::BTreeRelation;
use rex::eval::{self, Value};
use rex::events::dispatch_event;
use rex::types::typed::TProgram;
use std::collections::HashMap;

struct App {
    engine: Engine,
    env: rex::types::Env,
    events: Vec<rex::types::shape_ir::EventDef>,
}

fn build(src: &str) -> App {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "check: {:?}", checked.diagnostics);
    let prog: TProgram = checked.elaborated.unwrap();
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &prog.stmts {
        engine.apply_typed_stmt(stmt, &mut values);
    }
    App { engine, env: checked.env, events: checked.shapes.events }
}

fn errors(src: &str) -> Vec<String> {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    rex::check(&parsed.program).diagnostics.iter().map(|d| d.message.clone()).collect()
}

fn fire(app: &mut App, name: &str, args: &[(&str, Value)]) {
    let args: HashMap<String, ArgValue> = args
        .iter()
        .map(|(k, v)| (k.to_string(), ArgValue::Value(v.clone())))
        .collect();
    dispatch_event(&mut app.engine, &app.env, &app.events, name, &args)
        .unwrap_or_else(|e| panic!("dispatch `{name}`: {e}"));
}

fn view(app: &App, name: &str) -> BTreeRelation {
    app.engine.circuit.view(name).map(|v| v.to_relation()).unwrap_or_default()
}

/// The right-hand values of a view, semantically sorted — enough to say what
/// a `Unit`- or entity-keyed view currently holds.
fn values_of(app: &App, name: &str) -> Vec<Value> {
    let mut vs: Vec<Value> =
        view(app, name).triples().filter(|(_, _, w)| *w > 0).map(|(_, r, _)| r.clone()).collect();
    vs.sort_by(|a, b| a.cmp_semantic(b));
    vs
}

const TODOS: &str = r#"
type Filter = All | Active | Completed
entity Todo { text: Text, completed: Bool }
state filter : Filter = All

event SetFilter(f: Filter)
event AddTodo(text: Text, done: Bool)
on SetFilter(f)        => set filter = f
on AddTodo(text, done) => new Todo { text: text, completed: done }

let current : Todo -> Filter = filter
let texts   : Todo -> Text = .text
"#;

#[test]
fn a_state_starts_at_its_default() {
    // No todos yet, so `current` (keyed by Todo) is empty — but the state
    // itself is set: adding a todo immediately sees `@All`.
    let mut app = build(TODOS);
    fire(&mut app, "AddTodo", &[("text", Value::text("a")), ("done", Value::atom("False"))]);
    assert_eq!(values_of(&app, "current"), vec![Value::atom("All")]);
}

#[test]
fn set_changes_the_state_for_every_row() {
    let mut app = build(TODOS);
    fire(&mut app, "AddTodo", &[("text", Value::text("a")), ("done", Value::atom("False"))]);
    fire(&mut app, "AddTodo", &[("text", Value::text("b")), ("done", Value::atom("False"))]);
    assert_eq!(values_of(&app, "current"), vec![Value::atom("All"), Value::atom("All")]);

    fire(&mut app, "SetFilter", &[("f", Value::atom("Active"))]);
    assert_eq!(values_of(&app, "current"), vec![Value::atom("Active"), Value::atom("Active")]);
}

#[test]
fn a_state_change_is_one_field_delta_not_a_recompute() {
    // The acceptance line, checked the way it is observable from outside the
    // engine: `set filter = Active` writes one cell of `State#`, so a view
    // that does not read the state must not move at all, however many todos
    // are downstream. A recompute-on-state-change would light up every view.
    let mut app = build(TODOS);
    for i in 0..50 {
        fire(&mut app, "AddTodo", &[("text", Value::text(&format!("t{i}"))), ("done", Value::atom("False"))]);
    }
    let args: HashMap<String, ArgValue> =
        [("f".to_string(), ArgValue::Value(Value::atom("Active")))].into_iter().collect();
    let (_, step) = dispatch_event(&mut app.engine, &app.env, &app.events, "SetFilter", &args).unwrap();

    // Downstream, all 50 rows' `current` values move — that is inherent, the
    // relation really did change at every key (MVP-PLAN §2.10's fan-out).
    let current = step.view_deltas.get("current").expect("current delta");
    assert_eq!(current.triples().filter(|(_, _, w)| *w < 0).count(), 50);
    assert_eq!(current.triples().filter(|(_, _, w)| *w > 0).count(), 50);
    // But `texts` — a view that does not read the state — must not move at
    // all. If a state change were a recompute, it would show up here.
    assert!(
        step.view_deltas.get("texts").is_none_or(|d| d.is_empty()),
        "a state change disturbed a view that does not read it"
    );
}

#[test]
fn a_state_read_is_incremental_under_a_later_insert() {
    // The other direction: with the state already changed, a *new* row picks
    // up the current value through the same joins.
    let mut app = build(TODOS);
    fire(&mut app, "SetFilter", &[("f", Value::atom("Completed"))]);
    fire(&mut app, "AddTodo", &[("text", Value::text("late")), ("done", Value::atom("False"))]);
    assert_eq!(values_of(&app, "current"), vec![Value::atom("Completed")]);
}

#[test]
fn state_matches_the_batch_evaluator() {
    // The composite a state name desugars to is ordinary relational algebra,
    // so the batch evaluator computes the same thing from the same source.
    let src = r#"
entity Todo { text: Text }
state label : Text = "hello"
let t0 = new Todo { text: "a" }
let greet : Todo -> Text = label
"#;
    let app = build(src);
    let batch = eval::run(&rex::parse(src).program);
    assert_eq!(&view(&app, "greet"), batch.view("greet").expect("batch view"));
    assert_eq!(values_of(&app, "greet"), vec![Value::text("hello")]);
}

#[test]
fn a_state_can_be_read_in_the_value_it_is_set_to() {
    // The benchmark's `set nextId = nextId + n`: the read is against the
    // pre-event snapshot, so this is a well-defined increment, not a loop.
    let src = r#"
entity Row { pos: Int }
state nextId : Int = 1
event Bump(n: Int)
on Bump(n) {
  new Row { pos: nextId }
  set nextId = nextId + n
}
let positions : Row -> Int = .pos
"#;
    let mut app = build(src);
    fire(&mut app, "Bump", &[("n", Value::Int(10))]);
    fire(&mut app, "Bump", &[("n", Value::Int(10))]);
    fire(&mut app, "Bump", &[("n", Value::Int(10))]);
    // Rows minted at 1, 11, 21 — each `new` saw the value before its own
    // `set`, and each `set` saw the value before the same event's write.
    assert_eq!(values_of(&app, "positions"), vec![Value::Int(1), Value::Int(11), Value::Int(21)]);
}

#[test]
fn a_defaultless_state_starts_empty() {
    // Chat's `state current : User` — 0 rows until `set`, which is how "no
    // current user" is said without an option type. Reading it before then is
    // an error at dispatch, not a silent null.
    let src = r#"
entity User { name: Text }
entity Note { author: User }
state current : User
event Select(u: User)
event Write()
on Select(u) => set current = u
on Write()   => new Note { author: current }
let u0 = new User { name: "Ada" }
let authors : Note -> User = .author
"#;
    let mut app = build(src);
    let args: HashMap<String, ArgValue> = HashMap::new();
    let err = dispatch_event(&mut app.engine, &app.env, &app.events, "Write", &args)
        .expect_err("writing with no current user should fail");
    // Rejected, not invalid: the call is fine, the state does not allow it.
    assert_eq!(err, rex::events::Refusal::Rejected("state `current` has no value yet".to_string()));

    let ada = app.engine.circuit
        .input_integral(&rex::dbsp::InputKey::Identity(app.env.entity_sort("User").unwrap()))
        .unwrap()
        .triples()
        .find(|(_, _, w)| *w > 0)
        .map(|(l, _, _)| l.clone())
        .unwrap();
    fire(&mut app, "Select", &[("u", ada.clone())]);
    fire(&mut app, "Write", &[]);
    assert_eq!(values_of(&app, "authors"), vec![ada]);
}

#[test]
fn setting_an_undeclared_state_is_an_error() {
    let src = r#"
entity Todo { text: Text }
event Nope()
on Nope() => set missing = 1
"#;
    let errs = errors(src);
    assert!(errs.iter().any(|e| e.contains("unknown state `missing`")), "got {errs:?}");
}

#[test]
fn a_state_value_is_checked_against_its_declared_type() {
    let src = r#"
type Filter = All | Active
entity Todo { text: Text }
state filter : Filter = All
event Bad()
on Bad() => set filter = "nonsense"
"#;
    let errs = errors(src);
    assert!(errs.iter().any(|e| e.contains("expects")), "got {errs:?}");
}

#[test]
fn the_state_row_is_logged_as_genesis_so_replay_reproduces_it() {
    // The singleton is an ordinary top-level `new`, so S-21's genesis logging
    // already covers it: replaying the log into a fresh engine must restore
    // the state, including a value written by a later `set`.
    let mut app = build(TODOS);
    fire(&mut app, "AddTodo", &[("text", Value::text("a")), ("done", Value::atom("False"))]);
    fire(&mut app, "SetFilter", &[("f", Value::atom("Active"))]);

    let parsed = rex::parse(TODOS);
    let checked = rex::check(&parsed.program);
    let prog = checked.elaborated.unwrap();
    let mut fresh = Engine::new();
    let mut values = HashMap::new();
    for stmt in &prog.stmts {
        // Views only: the data comes back from the log, not from re-running
        // the program's `new` statements.
        if !matches!(stmt, rex::types::typed::TStmt::New { .. }) {
            fresh.apply_typed_stmt(stmt, &mut values);
        }
    }
    let log: Vec<_> = app.engine.log().to_vec();
    rex::events::replay(&mut fresh, &checked.env, &checked.shapes.events, &log, true).unwrap();

    let replayed = App { engine: fresh, env: checked.env, events: checked.shapes.events };
    assert_eq!(values_of(&replayed, "current"), vec![Value::atom("Active")]);
    assert_eq!(values_of(&replayed, "current"), values_of(&app, "current"));
}

#[test]
fn a_default_may_name_a_seed_row_wherever_the_state_is_declared() {
    // The state row is a `new` like any other, and can only refer to rows
    // created before it — so it is placed after the one its default names,
    // not first, even when the `state` line comes before the `let`.
    for src in [
        "entity User { name: Text }\nlet ada = new User { name: \"Ada\" }\nstate current : User = ada\nlet who : Unit -> Text = current . .name\n",
        "entity User { name: Text }\nstate current : User = ada\nstate n : Int = 3\nlet who : Unit -> Text = current . .name\nlet ada = new User { name: \"Ada\" }\n",
    ] {
        let app = build(src);
        let who = app.engine.circuit.view("who").unwrap().to_sorted_vec();
        assert_eq!(who, vec![(Value::Unit, Value::text("Ada"), 1)], "{src}");
        // …and a restoring boot rebuilds it from the log.
        let log = app.engine.log().to_vec();
        assert_eq!(log.len(), 2, "one genesis event per `new`");
    }
    // A default that names nothing in scope is still an error.
    let parsed = rex::parse("entity User { name: Text }\nstate current : User = nobody\n");
    assert!(rex::check(&parsed.program).elaborated.is_none());
}
