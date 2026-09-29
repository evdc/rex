//! S-53: `Unit`-root view levels and scalar binds.
//!
//! A `view` whose body is a bare element sits at an implicit level over the
//! single point `unit`, so static chrome and scalar binds (`count(Todo by
//! unit)`) need no entity. An `if (c) { … }` is a child level over the same
//! point, present exactly where `c` holds; several `select`s may sit under one
//! root. The shaper is unchanged: a count changing is one delta on the text
//! bind's view, at the root's one key.

use rex::dbsp::{ArgValue, Engine};
use rex::eval::relation::BTreeRelation;
use rex::eval::Value;
use rex::events::dispatch_event;
use rex::types::shape_ir::{BindKind, ShapeLevel, Tpl};
use rex::types::typed::TProgram;
use std::collections::HashMap;

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

fn errors(src: &str) -> Vec<String> {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    rex::check(&parsed.program).diagnostics.iter().map(|d| d.message.clone()).collect()
}

/// Fire an event, returning the per-view delta rows (weight-summed, zero rows
/// dropped) so a test can say exactly which views moved.
fn fire(app: &mut App, name: &str, args: &[(&str, Value)]) -> HashMap<String, Vec<(Value, Value, i64)>> {
    let args: HashMap<String, ArgValue> =
        args.iter().map(|(k, v)| (k.to_string(), ArgValue::Value(v.clone()))).collect();
    let (_, step) = dispatch_event(&mut app.engine, &app.env, &app.shapes.events, name, &args)
        .unwrap_or_else(|e| panic!("dispatch `{name}`: {e}"));
    step.view_deltas
        .into_iter()
        .map(|(v, r)| (v, r.triples().map(|(a, b, w)| (a.clone(), b.clone(), w)).collect::<Vec<_>>()))
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

fn view(app: &App, name: &str) -> BTreeRelation {
    app.engine.circuit.view(name).cloned().unwrap_or_default()
}

fn present(app: &App, name: &str) -> bool {
    view(app, name).triples().any(|(_, _, w)| w > 0)
}

const APP: &str = r#"
entity Todo { text: Text, completed: Bool }
event Add(text: Text)
event Done(t: Todo)
on Add(text) => new Todo { text: text, completed: False }
on Done(t)   => t.completed := True

let total  : Unit -> Int = count(Todo by unit)

view main =
  section(class="app") {
    h1 "todos"
    if (total > 0) {
      footer(class="footer") { span { total } " items" }
    }
    ul { Todo as t select li { .text } }
  }
"#;

fn find<'a>(l: &'a ShapeLevel, name: &str) -> Option<&'a ShapeLevel> {
    if l.name == name {
        return Some(l);
    }
    l.children.iter().find_map(|c| find(c, name))
}

#[test]
fn a_bare_element_view_is_one_level_over_unit() {
    let app = build(APP);
    let root = &app.shapes.views[0];
    assert_eq!(root.name, "main#unit");
    assert_eq!(root.entity, "Unit");
    assert_eq!(root.membership_view, "main#unit");
    match &root.template {
        Tpl::Elem { tag, classes, children, .. } => {
            assert_eq!(tag, "section");
            assert_eq!(classes, &["app"]);
            // `h1` and `ul` are static; the `if` and the `select` are levels.
            assert_eq!(children.len(), 2);
        }
        t => panic!("unexpected template {t:?}"),
    }
    assert_eq!(root.children.len(), 2, "one gate level and one select level");
    // Static chrome renders at once: the root membership is the one point.
    let rows: Vec<_> = view(&app, "main#unit").triples().map(|(a, b, _)| (a.clone(), b.clone())).collect();
    assert_eq!(rows, vec![(Value::Unit, Value::Unit)]);
}

#[test]
fn an_if_is_present_exactly_where_its_condition_holds() {
    let mut app = build(APP);
    let gate = app.shapes.views[0].children.iter().find(|c| c.name.contains("#if")).unwrap().name.clone();
    assert!(!present(&app, &gate), "no todos yet, so the footer is absent");

    let delta = fire(&mut app, "Add", &[("text", Value::text("a"))]);
    assert!(present(&app, &gate), "one todo, so the footer mounts");
    assert_eq!(delta[&gate], vec![(Value::Unit, Value::Unit, 1)], "one assertion at the root key");

    // A second todo changes the count but not the gate: it stays mounted.
    let delta = fire(&mut app, "Add", &[("text", Value::text("b"))]);
    assert!(!delta.contains_key(&gate), "the gate did not move: {delta:?}");
}

#[test]
fn a_scalar_bind_updates_with_one_delta_at_the_root_key() {
    let mut app = build(APP);
    let gate = app.shapes.views[0].children.iter().find(|c| c.name.contains("#if")).unwrap();
    let footer = find(gate, &gate.name).unwrap();
    let count = footer.attrs.iter().find(|a| a.kind == BindKind::Text).expect("the count's text bind");
    assert_eq!(count.path, vec![0], "bound to the span, child 0 of the footer");
    let bind = count.view.clone();

    fire(&mut app, "Add", &[("text", Value::text("a"))]);
    let delta = fire(&mut app, "Add", &[("text", Value::text("b"))]);
    // −1 and +2 at the same key: the shaper fuses it to one text mutation.
    assert_eq!(
        delta[&bind],
        vec![(Value::Unit, Value::Int(1), -1), (Value::Unit, Value::Int(2), 1)]
    );
}

#[test]
fn several_selects_of_one_entity_get_distinct_levels() {
    let src = r#"
entity Todo { text: Text, completed: Bool }
view main =
  div {
    ul { Todo as a select li { .text } }
    ul { Todo as b select li { .text } }
  }
"#;
    let app = build(src);
    let names: Vec<_> = app.shapes.views[0].children.iter().map(|c| c.name.clone()).collect();
    assert_eq!(names, vec!["main#unit#todo", "main#unit#todo2"]);
}

#[test]
fn a_select_under_the_root_needs_no_membership_where() {
    let mut app = build(APP);
    let list = app.shapes.views[0].children.iter().find(|c| c.entity == "Todo").unwrap().name.clone();
    fire(&mut app, "Add", &[("text", Value::text("a"))]);
    let rows: Vec<_> = view(&app, &list).triples().map(|(_, parent, _)| parent.clone()).collect();
    assert_eq!(rows, vec![Value::Unit], "every row's parent is the one point");
}

#[test]
fn a_state_reads_at_the_root_level() {
    let src = r#"
type Filter = All | Active
state filter : Filter = All
event Set(f: Filter)
on Set(f) => set filter = f
view main = div { a(class.selected=(filter = All)) "All" }
"#;
    let mut app = build(src);
    let gate = app.shapes.views[0]
        .attrs
        .iter()
        .find(|a| matches!(a.kind, BindKind::Class(_)))
        .expect("the class bind")
        .view
        .clone();
    assert!(present(&app, &gate), "filter starts at All");
    let delta = fire(&mut app, "Set", &[("f", Value::atom("Active"))]);
    assert_eq!(delta[&gate], vec![(Value::Unit, Value::Unit, -1)]);
}

#[test]
fn an_if_body_may_only_hold_elements() {
    let errs = errors(
        r#"
entity Todo { text: Text }
let total : Unit -> Int = count(Todo by unit)
view main = div { if (total > 0) { "hi" } }
"#,
    );
    assert!(errs.iter().any(|e| e.contains("only elements")), "{errs:?}");
}
