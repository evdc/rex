//! S-91: engine cost of the js-framework-benchmark operations, natively.
//!
//! Not a pass/fail test (timings are machine-dependent): run
//! `cargo test --release -p rex --test bench_create -- --ignored --nocapture`
//! and read the microseconds. The wasm figure is in the browser spec.

use rex::dbsp::{ArgValue, Engine};
use rex::eval::Value;
use rex::events::dispatch_event;
use std::collections::HashMap;
use std::time::Instant;

fn labels(n: i64) -> ArgValue {
    ArgValue::Rel((0..n).map(|i| (Value::Int(i), Value::text(&format!("pretty red table {i}")), 1)).collect())
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
}
