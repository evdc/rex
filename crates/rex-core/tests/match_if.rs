//! S-52: `match`, `if … then … else`, relational `not`, and `Bool` in filter
//! position.
//!
//! All four expand to core forms and are then checked normally, so none of
//! them adds a node to the typed IR, the batch evaluator or the circuit — and
//! the oracle property tests cover them for free. The expansions:
//!
//! ```text
//! match S { P => R, …, _ => D }  ==  (S = P) . R | … | (id except ((S = P) | …)) . D
//! if C then A else B             ==  C . A | (id except C) . B
//! not P                          ==  id except P
//! where .bool                    ==  where .bool = True
//! ```
//!
//! Each arm's gate is a coreflexive on the ambient domain, so composing it
//! with the arm's body keeps the body where the gate holds and drops it
//! elsewhere; a `_` gate is the complement of every named one, so exactly one
//! arm holds at each key.

use rex::dbsp::{ArgValue, Engine};
use rex::eval::relation::{BTreeRelation, BinaryRelation};
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

/// The texts of the todos a `Todo`-coreflexive view currently selects.
fn selected(app: &App, view: &str) -> Vec<String> {
    let rel = app.engine.circuit.view(view).cloned().unwrap_or_default();
    let texts = app
        .engine
        .circuit
        .input_integral(&rex::dbsp::InputKey::Field(
            app.env.entity_sort("Todo").unwrap(),
            rex::eval::intern("text"),
        ))
        .cloned()
        .unwrap_or_default();
    let mut out: Vec<String> = rel
        .triples()
        .filter(|(_, _, w)| *w > 0)
        .filter_map(|(k, _, _)| {
            texts.row(k).find(|(_, w)| *w > 0).map(|(v, _)| match v {
                Value::Text(s) => s.as_str().to_string(),
                other => format!("{other:?}"),
            })
        })
        .collect();
    out.sort();
    out
}

fn assert_matches_batch(app: &App, src: &str, view: &str, context: &str) {
    let batch = eval::run(&rex::parse(src).program);
    assert_eq!(
        app.engine.circuit.view(view).unwrap_or(&BTreeRelation::new()),
        batch.view(view).expect("batch view"),
        "view `{view}` diverged from batch ({context})"
    );
}

const TODOMVC: &str = r#"
type Filter = All | Active | Completed
entity Todo { text: Text, completed: Bool }
state filter : Filter = All

event SetFilter(f: Filter)
event AddTodo(text: Text, done: Bool)
on SetFilter(f)        => set filter = f
on AddTodo(text, done) => new Todo { text: text, completed: done }

let visible : Todo = match filter {
  All       => Todo
  Active    => Todo where not .completed
  Completed => Todo where .completed
}
"#;

fn todos() -> App {
    let mut app = build(TODOMVC);
    fire(&mut app, "AddTodo", &[("text", Value::text("write")), ("done", Value::atom("False"))]);
    fire(&mut app, "AddTodo", &[("text", Value::text("ship")), ("done", Value::atom("True"))]);
    fire(&mut app, "AddTodo", &[("text", Value::text("rest")), ("done", Value::atom("False"))]);
    app
}

#[test]
fn match_selects_the_arm_the_scrutinee_names() {
    // TodoMVC's filter, the reason this story exists.
    let mut app = todos();
    assert_eq!(selected(&app, "visible"), vec!["rest", "ship", "write"]);

    fire(&mut app, "SetFilter", &[("f", Value::atom("Active"))]);
    assert_eq!(selected(&app, "visible"), vec!["rest", "write"]);

    fire(&mut app, "SetFilter", &[("f", Value::atom("Completed"))]);
    assert_eq!(selected(&app, "visible"), vec!["ship"]);

    fire(&mut app, "SetFilter", &[("f", Value::atom("All"))]);
    assert_eq!(selected(&app, "visible"), vec!["rest", "ship", "write"]);
}

#[test]
fn match_is_incremental_across_a_filter_flip() {
    // Flipping the filter is a membership change, so the delta must be the
    // rows that actually entered or left — not a teardown of the whole view.
    let mut app = todos();
    let args: HashMap<String, ArgValue> =
        [("f".to_string(), ArgValue::Value(Value::atom("Completed")))].into_iter().collect();
    let (_, step) = dispatch_event(&mut app.engine, &app.env, &app.events, "SetFilter", &args).unwrap();
    let delta = step.view_deltas.get("visible").expect("visible delta");
    // Three todos were visible under `All`; only "ship" stays. So: two
    // retractions, no assertions — "ship" was already there and must not be
    // re-emitted, which is what makes node identity survive in the DOM.
    assert_eq!(delta.triples().filter(|(_, _, w)| *w < 0).count(), 2);
    assert_eq!(delta.triples().filter(|(_, _, w)| *w > 0).count(), 0);
}

#[test]
fn match_matches_the_batch_evaluator() {
    let src = r#"
type Filter = All | Active | Completed
entity Todo { text: Text, completed: Bool }
state filter : Filter = Active
let t0 = new Todo { text: "a", completed: False }
let t1 = new Todo { text: "b", completed: True }
let visible : Todo = match filter {
  All       => Todo
  Active    => Todo where not .completed
  Completed => Todo where .completed
}
"#;
    let app = build(src);
    assert_matches_batch(&app, src, "visible", "match over a state");
}

#[test]
fn a_wildcard_arm_takes_every_key_the_others_did_not() {
    let src = r#"
type Filter = All | Active | Completed
entity Todo { text: Text, completed: Bool }
state filter : Filter = Completed
let t0 = new Todo { text: "a", completed: False }
let t1 = new Todo { text: "b", completed: True }
let visible : Todo = match filter {
  Active => Todo where not .completed
  _      => Todo
}
"#;
    let app = build(src);
    assert_eq!(selected(&app, "visible"), vec!["a", "b"]);
    assert_matches_batch(&app, src, "visible", "wildcard arm");
}

#[test]
fn a_wildcard_arm_must_come_last() {
    let src = r#"
type Filter = All | Active
entity Todo { text: Text }
state filter : Filter = All
let visible : Todo = match filter {
  _   => Todo
  All => Todo
}
"#;
    let errs = errors(src);
    assert!(errs.iter().any(|e| e.contains("`_` arm must come last")), "got {errs:?}");
}

#[test]
fn relational_not_is_the_complement_within_the_domain() {
    // `not P` is `id except P`, so it includes a row that has *no* value for
    // the field at all — not only one holding `False`. 6NF has no nulls, so
    // "absent" is a real state and the complement has to cover it.
    let src = r#"
entity Todo { text: Text, completed: Bool }
event AddPlain(text: Text)
on AddPlain(text) => new Todo { text: text }
let t0 = new Todo { text: "done", completed: True }
let t1 = new Todo { text: "open", completed: False }
let active : Todo = Todo where not .completed
"#;
    let mut app = build(src);
    assert_eq!(selected(&app, "active"), vec!["open"]);
    // Compared before dispatching: the batch evaluator only sees the source's
    // own `new`s, not rows a later event minted.
    assert_matches_batch(&app, src, "active", "not over a total field");

    // A todo with no `completed` value at all is also "not completed".
    fire(&mut app, "AddPlain", &[("text", Value::text("bare"))]);
    assert_eq!(selected(&app, "active"), vec!["bare", "open"]);
}

#[test]
fn a_bool_in_filter_position_means_equals_true() {
    // `where .completed` is `where .completed = True`. Without the coercion
    // the semijoin would keep every row that has *any* value there, i.e. the
    // completed and the uncompleted alike.
    let src = r#"
entity Todo { text: Text, completed: Bool }
let t0 = new Todo { text: "done", completed: True }
let t1 = new Todo { text: "open", completed: False }
let done : Todo = Todo where .completed
"#;
    let app = build(src);
    assert_eq!(selected(&app, "done"), vec!["done"]);
    assert_matches_batch(&app, src, "done", "bare Bool filter");
}

#[test]
fn if_then_else_picks_a_branch_per_key() {
    // The gate is evaluated per key, so one view can carry both branches at
    // once — this is a relation, not a scalar conditional.
    let src = r#"
entity Todo { text: Text, completed: Bool }
let t0 = new Todo { text: "done", completed: True }
let t1 = new Todo { text: "open", completed: False }
let label : Todo -> Text = if .completed then "yes" else "no"
"#;
    let app = build(src);
    let rel = app.engine.circuit.view("label").cloned().unwrap_or_default();
    let mut got: Vec<String> = rel
        .triples()
        .filter(|(_, _, w)| *w > 0)
        .map(|(_, v, _)| match v {
            Value::Text(s) => s.as_str().to_string(),
            other => format!("{other:?}"),
        })
        .collect();
    got.sort();
    assert_eq!(got, vec!["no", "yes"]);
    assert_matches_batch(&app, src, "label", "if/then/else per key");
}

#[test]
fn a_filter_that_is_neither_a_comparison_nor_a_bool_is_rejected() {
    let src = r#"
entity Todo { text: Text }
let bad : Todo = Todo where not .text
"#;
    let errs = errors(src);
    assert!(
        errs.iter().any(|e| e.contains("must be a comparison or a `Bool`")),
        "got {errs:?}"
    );
}

#[test]
fn not_over_a_foreign_domain_is_rejected() {
    // `not P` is `id except P`, so `P` has to range over the *ambient* keys.
    // Antijoining against another sort's keys matches nothing, which would
    // silently make `not` the identity instead of the complement.
    let src = r#"
entity Todo { text: Text }
entity Project { name: Text }
let bad : Todo = Todo where not (Project where .name = "x")
"#;
    let errs = errors(src);
    assert!(
        errs.iter().any(|e| e.contains("`not` join column mismatch")),
        "got {errs:?}"
    );
}
