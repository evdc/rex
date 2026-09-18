//! S-03: engine/shaper contract fixtures. Drives `fixtures/board.rex` through
//! a scripted Kanban-shaped history via `Engine::dispatch` and checks each
//! step's delta JSON (the same `{"views":{...}}` shape the wasm bridge
//! produces) against `fixtures/steps/*.json`.
//! `js/rex-dom/test/contract.test.ts` applies those same files through the
//! real `Shaper` with a `SpyDriver` and asserts DOM mutation counts — so the
//! engine and the shaper are conformance-tested against one set of files,
//! independent of any example app.
//!
//! Regenerate with `UPDATE_FIXTURES=1 cargo test -p rex --test contract_fixtures`.

use rex::dbsp::{DispatchOp, Engine};
use rex::eval::interp::lit_value;
use rex::eval::value::Value;
use rex::eval::{json_quote, rows_to_json, step_result_to_json};
use rex::types::typed::Lit;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const BOARD_SRC: &str = include_str!("fixtures/board.rex");

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/steps")
}

/// The full contents of every registered view as `{"views":{...}}` — the
/// same shape `RexApp::snapshot()` produces on the wasm side (`crates/
/// rex-wasm/src/lib.rs`), reproduced here so this test has no wasm
/// dependency.
fn snapshot_json(engine: &Engine) -> String {
    let mut names: Vec<&String> = engine.circuit.output_names().collect();
    names.sort();
    let views: Vec<String> = names
        .iter()
        .map(|n| {
            let rel = engine.circuit.view(n).expect("registered view");
            format!("{}:{}", json_quote(n), rows_to_json(rel))
        })
        .collect();
    format!(r#"{{"views":{{{}}}}}"#, views.join(","))
}

fn set(engine: &mut Engine, id: &Value, field: &str, value: Value) -> String {
    let (_, res) =
        engine.dispatch(&[DispatchOp::Set { id: id.clone(), updates: vec![(field.to_string(), value)] }]);
    step_result_to_json(&res)
}

/// Seed the fixture program, returning the engine and its `new`-bound ids.
fn seed() -> (Engine, HashMap<String, Value>) {
    let parsed = rex::parse(BOARD_SRC);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "check: {:?}", checked.diagnostics);
    let typed = checked.elaborated.expect("clean check");

    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &typed.stmts {
        engine.apply_typed_stmt(stmt, &mut values);
    }
    (engine, values)
}

/// Drive the scripted history, one step per fixture file. Each step
/// exercises exactly one of the shaper's documented −/+ fusion guarantees
/// (`js/rex-dom/test/shaper.test.ts`): rename → one `setText`, reorder/
/// reparent → one `insertBefore`, delete → one `removeChild`, insert → a
/// fresh mount.
fn scripted_steps() -> Vec<(&'static str, String)> {
    let (mut engine, values) = seed();
    let c1 = values["c1"].clone();
    let c2 = values["c2"].clone();
    let l_doing = values["l_doing"].clone();
    let Value::Id(card_sort, _) = c1 else { unreachable!("c1 is a Card id") };

    let mut steps = vec![("00-mount.json", snapshot_json(&engine))];

    // 1: rename c1's title.
    steps.push(("01-rename.json", set(&mut engine, &c1, "title", Value::text("Design the schema"))));

    // 2: reorder c2 within its list (pos "a1" -> "a", sorts before c1).
    steps.push(("02-reorder.json", set(&mut engine, &c2, "pos", Value::text("a"))));

    // 3: reparent c1 from l_todo to l_doing (drag across lists).
    steps.push(("03-reparent.json", set(&mut engine, &c1, "list", l_doing.clone())));

    // 4: delete c2.
    let (_, res4) = engine.dispatch(&[DispatchOp::Retract { id: c2 }]);
    steps.push(("04-delete.json", step_result_to_json(&res4)));

    // 5: insert a new card into l_doing (a fresh mount).
    let (_, res5) = engine.dispatch(&[DispatchOp::New {
        sort: card_sort,
        fields: vec![
            ("title".to_string(), Value::text("Write tests")),
            ("pos".to_string(), lit_value(&Lit::Str("b0".to_string()))),
            ("list".to_string(), l_doing),
        ],
    }]);
    steps.push(("05-insert.json", step_result_to_json(&res5)));

    steps
}

#[test]
fn contract_fixtures_match_the_scripted_history() {
    let steps = scripted_steps();
    let dir = fixtures_dir();

    if std::env::var_os("UPDATE_FIXTURES").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        for (name, json) in &steps {
            std::fs::write(dir.join(name), json).unwrap_or_else(|e| panic!("write {name}: {e}"));
        }
        return;
    }

    for (name, json) in &steps {
        let path = dir.join(name);
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!("missing fixture {path:?} — run with UPDATE_FIXTURES=1 to generate it")
        });
        assert_eq!(*json, expected, "{name} changed — rerun with UPDATE_FIXTURES=1 if intentional");
    }
}

/// The scripted history is a pure function of the fixture program (no
/// wall-clock/random inputs, and per-sort id minting is deterministic given
/// operation order) — regenerating from a fresh engine reproduces the exact
/// same bytes.
#[test]
fn scripted_history_is_deterministic() {
    assert_eq!(scripted_steps(), scripted_steps());
}
