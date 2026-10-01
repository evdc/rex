//! S-91: engine cost of the js-framework-benchmark operations, natively.
//!
//! Not a pass/fail test (timings are machine-dependent): run
//! `cargo test --release -p rex --test bench_create -- --ignored --nocapture`
//! and read the microseconds. The wasm figure is in the browser spec.

use rex::dbsp::{ArgValue, Circuit, Engine, NodeId};
use rex::eval::Value;
use rex::events::dispatch_event;
use std::collections::HashMap;
use std::time::Instant;

fn labels(n: i64) -> ArgValue {
    ArgValue::Rel((0..n).map(|i| (Value::Int(i), Value::text(&format!("pretty red table {i}")), 1)).collect())
}

/// P-4b: how the kept integrals are stored — columns vs general relations.
/// Run after the events too: a column that demoted shows up as general.
fn integrals(c: &Circuit) -> String {
    let kept: Vec<NodeId> = (0..c.node_count()).map(NodeId).filter(|&id| c.keeps_integral(id)).collect();
    let cols = kept.iter().filter(|&&id| c.integral(id).is_column()).count();
    format!("{} integrals kept: {cols} columns, {} general", kept.len(), kept.len() - cols)
}

fn args(pairs: Vec<(&str, ArgValue)>) -> HashMap<String, ArgValue> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[test]
#[ignore = "timing harness"]
fn engine_cost_of_each_operation() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/js-framework-benchmark/src/app.rex")).unwrap();
    let parsed = rex::parse(&src);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let prog = checked.elaborated.unwrap();
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &prog.stmts {
        engine.apply_typed_stmt(stmt, &mut values);
    }
    let c = &engine.circuit;
    println!("circuit: {} nodes, {} inputs; {}", c.node_count(), c.input_keys().count(), integrals(c));
    let mut run = |label: &str, name: &str, a: Vec<(&str, ArgValue)>| {
        let t = Instant::now();
        let (_, step) = dispatch_event(&mut engine, &checked.env, &checked.shapes.events, name, &args(a))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let us = t.elapsed().as_micros();
        let rows: usize = step.view_deltas.values().map(|r| r.triples().count()).sum();
        println!("{label:<28} {us:>9} µs   ({rows} delta rows)");
    };
    run("Run(1000)", "Run", vec![("n", ArgValue::Value(Value::Int(1000))), ("labels", labels(1000))]);
    run("Run(10000)", "Run", vec![("n", ArgValue::Value(Value::Int(10000))), ("labels", labels(10000))]);
    run("Run(10000) again (replace)", "Run", vec![("n", ArgValue::Value(Value::Int(10000))), ("labels", labels(10000))]);
    run("Add(1000)", "Add", vec![("labels", labels(1000))]);
    run("Update()", "Update", vec![]);
    run("SwapRows()", "SwapRows", vec![]);
    run("Clear()", "Clear", vec![]);
    println!("after: {}", integrals(&engine.circuit));
}

/// P-2: TodoMVC has real intermediate nodes (filter joins, counts), so it is
/// where keeping integrals only on demand shows. Same harness shape as above.
#[test]
#[ignore = "timing harness"]
fn todomvc_engine_cost() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/todomvc/src/app.rex")).unwrap();
    let parsed = rex::parse(&src);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let prog = checked.elaborated.unwrap();
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &prog.stmts {
        engine.apply_typed_stmt(stmt, &mut values);
    }
    println!("circuit: {} nodes; {}", engine.circuit.node_count(), integrals(&engine.circuit));
    let mut dispatch = |name: &str, a: Vec<(&str, ArgValue)>| {
        dispatch_event(&mut engine, &checked.env, &checked.shapes.events, name, &args(a))
            .unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    let t = Instant::now();
    for i in 0..2000 {
        dispatch("AddTodo", vec![("text", ArgValue::Value(Value::text(&format!("todo {i}"))))]);
    }
    println!("{:<28} {:>9} µs", "AddTodo ×2000", t.elapsed().as_micros());
    let mut run = |label: &str, name: &str, a: Vec<(&str, ArgValue)>| {
        let t = Instant::now();
        let (_, step) = dispatch(name, a);
        let us = t.elapsed().as_micros();
        let rows: usize = step.view_deltas.values().map(|r| r.triples().count()).sum();
        println!("{label:<28} {us:>9} µs   ({rows} delta rows)");
    };
    run("ToggleAll(True)", "ToggleAll", vec![("done", ArgValue::Value(Value::atom("True")))]);
    run("SetFilter(Active)", "SetFilter", vec![("f", ArgValue::Value(Value::atom("Active")))]);
    run("ToggleAll(False)", "ToggleAll", vec![("done", ArgValue::Value(Value::atom("False")))]);
    run("ClearCompleted() (none)", "ClearCompleted", vec![]);
    run("ToggleAll(True) again", "ToggleAll", vec![("done", ArgValue::Value(Value::atom("True")))]);
    run("ClearCompleted() (2k)", "ClearCompleted", vec![]);
    println!("after: {}", integrals(&engine.circuit));
}
