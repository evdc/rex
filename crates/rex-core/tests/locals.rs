//! S-62: `local` state in a component — a relation keyed by the component's
//! row, defaulted on read, set by an implicit logged event.

use rex::dbsp::{ArgValue, Engine};
use rex::eval::relation::BinaryRelation;
use rex::eval::Value;
use rex::events::{dispatch_event, replay};
use rex::types::typed::TProgram;
use std::collections::HashMap;

const TODOMVC: &str = include_str!("../../../examples/todomvc/src/app.rex");
const EDIT_EVENT: &str = "local#TodoItem#editing#set";

struct App {
    engine: Engine,
    env: rex::types::Env,
    shapes: rex::types::shape_ir::ShapeProgram,
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
    App { engine, env: checked.env, shapes: checked.shapes }
}

fn fire(app: &mut App, name: &str, args: &[(&str, Value)]) -> Vec<Value> {
    let args: HashMap<String, ArgValue> =
        args.iter().map(|(k, v)| (k.to_string(), ArgValue::Value(v.clone()))).collect();
    dispatch_event(&mut app.engine, &app.env, &app.shapes.events, name, &args)
        .unwrap_or_else(|e| panic!("dispatch `{name}`: {e}"))
        .0
}

fn set_editing(app: &mut App, todo: &Value, on: bool) {
    let v = Value::atom(if on { "True" } else { "False" });
    fire(app, EDIT_EVENT, &[("t", todo.clone()), ("#value", v)]);
}

/// The class-gate view behind `class.editing=editing`.
fn editing_gate(app: &App) -> String {
    fn find(l: &rex::types::shape_ir::ShapeLevel) -> Option<String> {
        use rex::types::shape_ir::BindKind;
        l.attrs
            .iter()
            .find(|a| matches!(&a.kind, BindKind::Class(c) if c == "editing"))
            .map(|a| a.view.clone())
            .or_else(|| l.children.iter().find_map(find))
    }
    app.shapes.views.iter().find_map(find).expect("the `editing` class gate")
}

fn gated(app: &App, gate: &str, todo: &Value) -> bool {
    app.engine.circuit.view(gate).is_some_and(|v| v.row(todo).any(|(_, w)| w > 0))
}

fn add(app: &mut App, text: &str) -> Value {
    fire(app, "AddTodo", &[("text", Value::text(text))]);
    // The newest todo: the one no earlier call returned; ids are minted in order.
    let ids = app.engine.circuit.view("visible").map(|v| v.domain().collect::<Vec<_>>()).unwrap_or_default();
    ids.into_iter().max_by(|a, b| a.cmp_semantic(b)).expect("a todo")
}

#[test]
fn todomvc_checks_with_local_state() {
    build(TODOMVC);
}

#[test]
fn a_local_defaults_and_is_set_per_row() {
    let mut app = build(TODOMVC);
    let gate = editing_gate(&app);
    let a = add(&mut app, "a");
    let b = add(&mut app, "b");
    assert!(!gated(&app, &gate, &a) && !gated(&app, &gate, &b), "default is False for every row");

    set_editing(&mut app, &a, true);
    assert!(gated(&app, &gate, &a), "a is editing");
    assert!(!gated(&app, &gate, &b), "b is untouched");

    set_editing(&mut app, &a, false);
    assert!(!gated(&app, &gate, &a), "setting it back to the default turns the class off");
}

#[test]
fn setting_a_local_is_one_step_on_one_field() {
    let mut app = build(TODOMVC);
    let a = add(&mut app, "a");
    let args: HashMap<String, ArgValue> = [
        ("t".to_string(), ArgValue::Value(a)),
        ("#value".to_string(), ArgValue::Value(Value::atom("True"))),
    ]
    .into();
    let (_, step) = dispatch_event(&mut app.engine, &app.env, &app.shapes.events, EDIT_EVENT, &args).unwrap();
    let moved: Vec<_> = step.view_deltas.iter().filter(|(_, r)| r.triples().next().is_some()).map(|(v, _)| v.clone()).collect();
    // Only the local's own views move — not the visible list, not the counts.
    assert!(moved.iter().all(|v| v.contains("editing") || v.contains("gate")), "{moved:?}");
}

#[test]
fn a_local_dies_with_its_row() {
    let mut app = build(TODOMVC);
    let gate = editing_gate(&app);
    let a = add(&mut app, "a");
    set_editing(&mut app, &a, true);
    fire(&mut app, "DeleteTodo", &[("t", a.clone())]);
    assert!(!gated(&app, &gate, &a));
}

#[test]
fn a_local_is_logged_and_replays() {
    let mut app = build(TODOMVC);
    let gate = editing_gate(&app);
    let a = add(&mut app, "a");
    set_editing(&mut app, &a, true);
    assert!(app.engine.log().iter().any(|e| e.name == EDIT_EVENT), "the set is a logged event");

    // A fresh engine with the views registered but no base data, then the
    // whole log (genesis included) replayed onto it.
    let parsed = rex::parse(TODOMVC);
    let checked = rex::check(&parsed.program);
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &checked.elaborated.as_ref().unwrap().stmts {
        if !matches!(stmt, rex::types::typed::TStmt::New { .. }) {
            engine.apply_typed_stmt(stmt, &mut values);
        }
    }
    replay(&mut engine, &checked.env, &checked.shapes.events, app.engine.log(), true).expect("replay");
    let fresh = App { engine, env: checked.env, shapes: checked.shapes };
    assert!(gated(&fresh, &gate, &a), "editing survives a replay");
}

#[test]
fn a_local_without_a_component_parameter_is_an_error() {
    let src = "entity Todo { text: Text }\nview main = local editing = False\n  div { .text }\n";
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let errs: Vec<_> = rex::check(&parsed.program).diagnostics.iter().map(|d| d.message.clone()).collect();
    assert!(errs.iter().any(|e| e.contains("key it by")), "{errs:?}");
}
