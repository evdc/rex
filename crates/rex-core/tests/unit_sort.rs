//! S-50: the `Unit` sort, the `unit` constant relation, and the *total*
//! aggregate that makes a global counter a real relation.
//!
//! The point of `by unit` is that it regroups everything under `Unit`'s single
//! point, so `count(Todo by unit)` is the global count — and, crucially, it is
//! `0` rather than a missing row when there are no todos. That decides the
//! README's open "empty group produces no row" question in the `by unit`
//! direction only: a group key that exists by construction gets the monoid
//! identity; a key that only exists because some row produced it does not.
//!
//! Every incremental assertion here is also checked against the batch
//! evaluator, since the two kernels have to agree (SPEC §10, `tests/dbsp.rs`).

use rex::dbsp::Engine;
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

/// The single value of a `Unit -> V` view, or `None` when the view has no row
/// at the `Unit` key at all — the distinction this story is about.
fn at_unit(app: &App, view: &str) -> Option<Value> {
    app.engine
        .circuit
        .view(view)
        .map(|v| v.to_relation()).as_ref().unwrap_or(&BTreeRelation::new())
        .row(&Value::Unit)
        .find(|(_, w)| *w > 0)
        .map(|(v, _)| v)
}

fn fire(app: &mut App, name: &str, args: &[(&str, Value)]) {
    let args: HashMap<String, rex::dbsp::ArgValue> = args
        .iter()
        .map(|(k, v)| (k.to_string(), rex::dbsp::ArgValue::Value(v.clone())))
        .collect();
    dispatch_event(&mut app.engine, &app.env, &app.events, name, &args)
        .unwrap_or_else(|e| panic!("dispatch `{name}`: {e}"));
}

/// Cross-check a `Unit`-keyed view against a fresh batch evaluation of the
/// same source: the incremental answer must be the batch answer, including
/// when the batch answer is "the identity" and when it is "no row".
fn assert_matches_batch(app: &App, batch_src: &str, view: &str, context: &str) {
    let batch = eval::run(&rex::parse(batch_src).program);
    assert_eq!(
        app.engine.circuit.view(view).map(|v| v.to_relation()).as_ref().unwrap_or(&BTreeRelation::new()),
        batch.view(view).expect("batch view"),
        "view `{view}` diverged from batch ({context})"
    );
}

const COUNTER: &str = r#"
entity Todo { text: Text, done: {@yes | @no} }
event AddTodo(text: Text)
event DeleteTodo(t: Todo)
on AddTodo(text) => new Todo { text: text, done: @no }
on DeleteTodo(t) => delete t
let total : Unit -> Int = count(Todo by unit)
"#;

#[test]
fn count_by_unit_is_zero_when_empty() {
    // The acceptance line: no todos at all, and `total` is still a relation
    // with a row — `0` — not an absent key.
    let app = build(COUNTER);
    assert_eq!(at_unit(&app, "total"), Some(Value::Int(0)));
    assert_matches_batch(&app, COUNTER, "total", "empty");
}

#[test]
fn count_by_unit_tracks_inserts_and_retracts_incrementally() {
    let mut app = build(COUNTER);
    assert_eq!(at_unit(&app, "total"), Some(Value::Int(0)));

    fire(&mut app, "AddTodo", &[("text", Value::text("write the spec"))]);
    assert_eq!(at_unit(&app, "total"), Some(Value::Int(1)));

    fire(&mut app, "AddTodo", &[("text", Value::text("and the tests"))]);
    assert_eq!(at_unit(&app, "total"), Some(Value::Int(2)));

    // Back down to empty: the identity has to come *back*, which is the case
    // a naive "seed the key once" implementation gets wrong — the key must
    // stay present in the aggregate's state rather than being dropped when
    // its image empties out.
    let todos: Vec<Value> = app
        .engine
        .circuit
        .view("total")
        .map(|_| ())
        .into_iter()
        .flat_map(|()| {
            let sort = app.env.entity_sort("Todo").unwrap();
            app.engine
                .circuit
                .input_integral(&rex::dbsp::InputKey::Identity(sort))
                .unwrap()
                .triples()
                .filter(|(_, _, w)| *w > 0)
                .map(|(l, _, _)| l.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(todos.len(), 2);

    fire(&mut app, "DeleteTodo", &[("t", todos[0].clone())]);
    assert_eq!(at_unit(&app, "total"), Some(Value::Int(1)));

    fire(&mut app, "DeleteTodo", &[("t", todos[1].clone())]);
    assert_eq!(at_unit(&app, "total"), Some(Value::Int(0)));
}

#[test]
fn count_by_unit_backfills_over_existing_rows() {
    // The other direction: rows exist *before* the view does, so the count
    // arrives by backfill rather than by delta.
    let src = r#"
entity Todo { text: Text }
let t0 = new Todo { text: "a" }
let t1 = new Todo { text: "b" }
let total : Unit -> Int = count(Todo by unit)
"#;
    let app = build(src);
    assert_eq!(at_unit(&app, "total"), Some(Value::Int(2)));
    assert_matches_batch(&app, src, "total", "backfill");
}

#[test]
fn sum_by_unit_is_zero_when_empty_but_min_has_no_identity() {
    // `Sum` and `Count` are monoids, so a total group emits their identity.
    // `Min`/`Max` have none and `Avg` is not a monoid at all, so those stay
    // absent — anything else would have to invent a value.
    let src = r#"
entity Item { n: Int }
let total : Unit -> Int = sum(Item.n by unit)
let smallest : Unit -> Int = min(Item.n by unit)
let mean : Unit -> Money = avg(Item.n by unit)
"#;
    let app = build(src);
    assert_eq!(at_unit(&app, "total"), Some(Value::Int(0)));
    assert_eq!(at_unit(&app, "smallest"), None);
    assert_eq!(at_unit(&app, "mean"), None);
    for view in ["total", "smallest", "mean"] {
        assert_matches_batch(&app, src, view, "empty, mixed aggregates");
    }
}

#[test]
fn an_ordinary_group_key_still_emits_no_row_when_empty() {
    // The contrast case that keeps the rule honest: grouping by a field is
    // not a total domain, so an empty relation yields an empty view — no
    // key to emit an identity at.
    let src = r#"
entity Todo { text: Text, list: Text }
let per_list : Text -> Int = count(Todo by .list)
"#;
    let app = build(src);
    assert!(app.engine.circuit.view("per_list").map(|v| v.to_relation()).as_ref().unwrap_or(&BTreeRelation::new()).is_empty());
    assert_matches_batch(&app, src, "per_list", "empty, grouped by a field");
}

#[test]
fn unit_needs_a_domain_to_ground_in() {
    // `unit` is a constant relation `X -> Unit`, so it needs to know what `X`
    // is, exactly like `"abc"` or `@atom` in the same position.
    let errs = errors("let u = unit\n");
    assert!(
        errs.iter().any(|e| e.contains("`unit` needs a known domain")),
        "expected a grounding error, got {errs:?}"
    );
}

#[test]
fn unit_is_a_builtin_name_not_a_reserved_word() {
    // Nothing stops a program from binding `unit` itself; the built-in only
    // fills in when the name is free.
    let src = r#"
entity Todo { text: Text }
let unit : Todo -> Text = .text
let echo : Todo -> Text = unit
"#;
    assert!(errors(src).is_empty(), "{:?}", errors(src));
}

// --- S-50 (2/2): `type` declarations ---------------------------------------
//
// A constructor names its atom verbatim: `type Filter = All | ...` makes `All`
// mean `@All`. Nothing is lowercased, so a constructor and a hand-written atom
// literal never silently become the same thing, and the mapping is reversible.

#[test]
fn a_type_declares_a_coproduct_of_its_constructors() {
    let src = r#"
type Filter = All | Active | Completed
entity Todo { text: Text, shown: Filter }
let shown : Todo -> Filter = .shown
"#;
    assert!(errors(src).is_empty(), "{:?}", errors(src));
}

#[test]
fn a_constructor_is_its_atom_written_without_the_at_sign() {
    // `@All` and `All` are the same value, so the two spellings are
    // interchangeable and a filter written either way type-checks.
    let src = r#"
type Filter = All | Active | Completed
entity Todo { text: Text, shown: Filter }
let bare : Todo -> Filter = (Todo where .shown = All) . .shown
let at   : Todo -> Filter = (Todo where .shown = @All) . .shown
"#;
    assert!(errors(src).is_empty(), "{:?}", errors(src));
}

#[test]
fn bool_is_predeclared() {
    // MVP-PLAN §5 decision 3, with this story's verbatim spelling: `Bool` is
    // sugar for `True | False`, i.e. `{@True | @False}`.
    let src = r#"
entity Todo { text: Text, completed: Bool }
let done : Todo -> Bool = .completed
let finished : Todo = Todo where .completed = True
"#;
    assert!(errors(src).is_empty(), "{:?}", errors(src));
}

#[test]
fn not_flips_a_declared_two_constructor_type() {
    // The payoff for `Bool` no longer being an opaque atom: `not` knows which
    // two atoms to flip between, so TodoMVC's toggle checks.
    let src = r#"
entity Todo { text: Text, completed: Bool }
event ToggleTodo(t: Todo)
on ToggleTodo(t) => t.completed := not t.completed
"#;
    assert!(errors(src).is_empty(), "{:?}", errors(src));
}

#[test]
fn a_constructor_belongs_to_only_one_type() {
    // Two types sharing a constructor would make a bare `Done` ambiguous,
    // since the atom it names carries no type tag.
    let src = r#"
type Status = Open | Done
type Task = Todo | Done
"#;
    let errs = errors(src);
    assert!(
        errs.iter().any(|e| e.contains("constructor `Done` is already declared")),
        "expected a clash error, got {errs:?}"
    );
}

#[test]
fn redeclaring_a_type_is_an_error() {
    let errs = errors("type Filter = All | Active\ntype Filter = One | Two\n");
    assert!(
        errs.iter().any(|e| e.contains("type `Filter` is already defined")),
        "got {errs:?}"
    );
}

#[test]
fn a_constructor_is_shadowed_by_a_binding_of_the_same_name() {
    // Same rule as `unit`: the built-in meaning only applies to a free name.
    let src = r#"
type Filter = All | Active
entity Todo { text: Text }
let All : Todo -> Text = .text
let echo : Todo -> Text = All
"#;
    assert!(errors(src).is_empty(), "{:?}", errors(src));
}

#[test]
fn a_declared_type_works_as_an_event_parameter() {
    // The event boundary has to know a declared type too, or `type` is only
    // half implemented: `SetFilter` carries an atom across it.
    let src = r#"
type Filter = All | Active | Completed
entity Todo { text: Text, shown: Filter }
event SetFilter(t: Todo, f: Filter)
on SetFilter(t, f) => t.shown := f
"#;
    assert!(errors(src).is_empty(), "{:?}", errors(src));
}

#[test]
fn a_field_of_declared_type_is_not_mistaken_for_an_entity() {
    // `shown: Filter` used to look like a foreign key to an entity called
    // `Filter`, which would walk a field path into nothing.
    let src = r#"
type Filter = All | Active
entity Todo { text: Text, shown: Filter }
event Show(t: Todo)
on Show(t) => t.shown := All
"#;
    assert!(errors(src).is_empty(), "{:?}", errors(src));
}
