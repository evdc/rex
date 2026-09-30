//! S-91: per-row values in bulk mutations, and `import js` extractors.
//!
//! `new … from rows as (i, v) { num: nextId + i }` binds the row's key/value
//! per minted row; `update Row where P { label: .label ++ "!" }` reads the row
//! being updated; both against the pre-event snapshot.

use rex::dbsp::{ArgValue, Engine};
use rex::eval::Value;
use rex::events::dispatch_event;
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
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &checked.elaborated.unwrap().stmts {
        engine.apply_typed_stmt(stmt, &mut values);
    }
    App { engine, env: checked.env, shapes: checked.shapes }
}

fn errors(src: &str) -> Vec<String> {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    rex::check(&parsed.program).diagnostics.iter().map(|d| d.message.clone()).collect()
}

fn fire(app: &mut App, name: &str, args: Vec<(&str, ArgValue)>) {
    let args: HashMap<String, ArgValue> = args.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    dispatch_event(&mut app.engine, &app.env, &app.shapes.events, name, &args)
        .unwrap_or_else(|e| panic!("dispatch `{name}`: {e}"));
}

fn labels(xs: &[&str]) -> ArgValue {
    ArgValue::Rel(xs.iter().enumerate().map(|(i, s)| (Value::Int(i as i64), Value::text(s), 1)).collect())
}

/// A view's right column, as display strings, sorted by row id.
fn column(app: &App, view: &str) -> Vec<String> {
    let mut v: Vec<_> = app
        .engine
        .circuit
        .view(view)
        .unwrap()
        .triples()
        .filter(|(_, _, w)| *w > 0)
        .map(|(l, r, _)| (l.clone(), r.to_string()))
        .collect();
    v.sort();
    v.into_iter().map(|(_, r)| r).collect()
}

const SRC: &str = r#"
entity Row { num: Int, label: Text, pos: Int }
state nextId : Int = 1
let nums   : Row -> Int  = .num
let labels : Row -> Text = .label
let poss   : Row -> Int  = .pos

event Run(n: Int, labels: Int -> Text)
event Bang()
on Run(n, labels) {
  new Row from labels as (i, label) { num: nextId + i, label: label, pos: i + 1 }
  set nextId = nextId + n
}
on Bang() => update Row where .num % 2 = 1 { label: .label ++ "!", pos: .pos + .num }
"#;

#[test]
fn a_from_binder_is_a_per_row_value_in_a_compound_field() {
    let mut app = build(SRC);
    fire(&mut app, "Run", vec![("n", ArgValue::Value(Value::Int(3))), ("labels", labels(&["a", "b", "c"]))]);
    fire(&mut app, "Run", vec![("n", ArgValue::Value(Value::Int(2))), ("labels", labels(&["d", "e"]))]);
    let nums = column(&app, "nums");
    assert_eq!(nums, vec!["1", "2", "3", "4", "5"], "nextId + i, with nextId advanced between events");
}

#[test]
fn a_bulk_update_reads_the_row_it_updates() {
    let mut app = build(SRC);
    fire(&mut app, "Run", vec![("n", ArgValue::Value(Value::Int(4))), ("labels", labels(&["a", "b", "c", "d"]))]);
    fire(&mut app, "Bang", vec![]);
    let labels = column(&app, "labels");
    assert_eq!(labels, vec!["\"a!\"", "\"b\"", "\"c!\"", "\"d\""], "rows 1 and 3 only, each from its own label");
    let poss = column(&app, "poss");
    assert_eq!(poss, vec!["2", "2", "6", "4"], "pos := .pos + .num reads two fields of the same row");
}

#[test]
fn concat_needs_text() {
    let errs = errors(&SRC.replace(r#".label ++ "!""#, r#".label ++ 1"#));
    assert!(errs.iter().any(|e| e.contains("`++` needs `Text`")), "{errs:?}");
}

const JS: &str = r#"
import js "./utils.js" as utils
entity Row { label: Text }
event Run(labels: Int -> Text)
on Run(labels) { new Row from labels as (i, l) { label: l } }
view main = button(on click(rows = utils.randomLabels(3)) => do Run(rows)) "go"
"#;

#[test]
fn a_js_extractor_takes_its_type_from_the_event_it_feeds() {
    let errs = errors(JS);
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn a_js_extractor_needs_a_declared_import_and_literal_args() {
    let errs = errors(&JS.replace(r#"import js "./utils.js" as utils"#, ""));
    assert!(errs.iter().any(|e| e.contains("not an `import js` module")), "{errs:?}");
    let errs = errors(&JS.replace("randomLabels(3)", "randomLabels(x)"));
    assert!(errs.iter().any(|e| e.contains("must be literals")), "{errs:?}");
}

#[test]
fn an_unused_js_param_needs_an_annotation() {
    let errs = errors(&JS.replace("do Run(rows)", "do Nothing()").replace("event Run(", "event Nothing()\nevent Run("));
    assert!(errs.iter().any(|e| e.contains("needs a type annotation")), "{errs:?}");
}
