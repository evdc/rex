//! End-to-end tests for the incremental engine over the SPEC §12 fixture:
//! backfill (views over existing data), delta-at-a-time (data arriving after
//! the views), and retraction — always compared against the batch evaluator.

use rex::dbsp::Engine;
use rex::eval::interp::lit_value;
use rex::eval::relation::{BTreeRelation, BinaryRelation};
use rex::eval::{self, Value};
use rex::types::typed::{TProgram, TStmt, TValue};
use std::collections::HashMap;

const SPEC12: &str = include_str!("fixtures/spec12.rex");
const VIEWS: [&str; 4] = ["lineprice", "custspend", "inregion", "result"];

fn elaborate(src: &str) -> TProgram {
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "parse: {:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "check: {:?}", checked.diagnostics);
    checked.elaborated.expect("clean check produces the elaborated program")
}

/// Apply one typed statement to the engine, mirroring the batch interpreter's
/// statement loop (this is what the Session will do per committed REPL line).
fn apply_stmt(engine: &mut Engine, stmt: &TStmt, values: &mut HashMap<String, Value>) {
    match stmt {
        TStmt::New { name, sort, fields } => {
            let resolved: Vec<(String, Value)> = fields
                .iter()
                .map(|(f, tv)| {
                    let v = match tv {
                        TValue::Lit(lit) => lit_value(lit),
                        TValue::Ref(n) => values[n].clone(),
                    };
                    (f.clone(), v)
                })
                .collect();
            let (id, _) = engine.apply_new(*sort, &resolved);
            if let Some(name) = name {
                values.insert(name.clone(), id);
            }
        }
        TStmt::Let { name, body } => {
            if let Some(name) = name {
                engine.add_view(name, body, values);
            }
        }
    }
}

fn assert_views_match_batch(engine: &Engine, batch_src: &str, context: &str) {
    let batch = eval::run(&rex::parse(batch_src).program);
    for name in VIEWS {
        assert_eq!(
            engine.circuit.view(name).unwrap_or(&BTreeRelation::new()),
            batch.view(name).expect("batch view"),
            "view `{name}` diverged from batch ({context})"
        );
    }
}

/// Split the fixture into entity declarations, `new` statements, and view
/// `let`s (source lines), preserving order.
fn fixture_lines() -> (Vec<&'static str>, Vec<&'static str>, Vec<&'static str>) {
    let mut entities = Vec::new();
    let mut news = Vec::new();
    let mut views = Vec::new();
    for line in SPEC12.lines() {
        let t = line.trim();
        if t.starts_with("entity") {
            entities.push(line);
        } else if t.contains("= new ") {
            news.push(line);
        } else if t.starts_with("let") {
            views.push(line);
        }
    }
    (entities, news, views)
}

#[test]
fn spec12_backfill_matches_batch() {
    // Fixture order: all data first, then the views — every `add_view`
    // backfills its whole subgraph over existing base tables.
    let typed = elaborate(SPEC12);
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &typed.stmts {
        apply_stmt(&mut engine, stmt, &mut values);
    }
    assert_views_match_batch(&engine, SPEC12, "backfill order");
}

#[test]
fn spec12_delta_at_a_time_matches_batch() {
    // Reverse order: define every view over EMPTY tables, then feed the ten
    // `new` statements one transaction at a time. After every step, the
    // circuit must agree with a batch run over the data-so-far. (The batch
    // oracle program puts its `let`s after the data — the batch evaluator is
    // definition-order-sensitive, the circuit must not be.)
    let (entities, news, views) = fixture_lines();
    let typed = elaborate(SPEC12);
    let mut engine = Engine::new();
    let mut values = HashMap::new();

    for stmt in &typed.stmts {
        if matches!(stmt, TStmt::Let { .. }) {
            apply_stmt(&mut engine, stmt, &mut values);
        }
    }
    let new_stmts: Vec<&TStmt> =
        typed.stmts.iter().filter(|s| matches!(s, TStmt::New { .. })).collect();
    assert_eq!(new_stmts.len(), news.len());

    for k in 0..new_stmts.len() {
        apply_stmt(&mut engine, new_stmts[k], &mut values);
        let oracle_src = format!(
            "{}\n{}\n{}\n",
            entities.join("\n"),
            news[..=k].join("\n"),
            views.join("\n"),
        );
        assert_views_match_batch(&engine, &oracle_src, &format!("after new #{k}"));
    }
}

#[test]
fn spec12_retraction() {
    let typed = elaborate(SPEC12);
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &typed.stmts {
        apply_stmt(&mut engine, stmt, &mut values);
    }

    let alice = values["alice"].clone();
    let bob = values["bob"].clone();
    let money = |cents: i64| Value::Money(cents);

    // Full data: alice 3*999 + 1*2450 + 2*999 = 7445; bob 5*2450 = 12250.
    let custspend = |e: &Engine| e.circuit.view("custspend").unwrap().to_sorted_vec();
    assert_eq!(
        custspend(&engine),
        vec![(alice.clone(), money(7445), 1), (bob.clone(), money(12250), 1)]
    );

    // Retract the o2 line (Line id 2, minted third): alice drops by 2*999.
    let line_sort = typed
        .stmts
        .iter()
        .find_map(|s| match s {
            TStmt::New { sort, fields, .. } if fields.iter().any(|(f, _)| f == "qty") => {
                Some(*sort)
            }
            _ => None,
        })
        .expect("Line sort");
    let result = engine.retract_entity(&Value::Id(line_sort, 2));
    assert_eq!(
        result.view_deltas["custspend"].to_sorted_vec(),
        vec![(alice.clone(), money(5447), 1), (alice.clone(), money(7445), -1)],
        "retract/assert pair for alice's new total"
    );
    assert_eq!(
        custspend(&engine),
        vec![(alice.clone(), money(5447), 1), (bob.clone(), money(12250), 1)]
    );

    // Retract order o2 itself: its only line is already gone, nothing changes.
    engine.retract_entity(&values["o2"]);
    assert_eq!(
        custspend(&engine),
        vec![(alice.clone(), money(5447), 1), (bob.clone(), money(12250), 1)]
    );

    // Retract bob: his identity and fields go, so `inregion` (built on `id`)
    // and `result` lose him — but `custspend` keys off the Line -> Order ->
    // customer field path, and o3 still dangles at bob's id. No FK cascade:
    // the dangling group survives, it just no longer joins with the entity.
    engine.retract_entity(&bob);
    assert_eq!(
        engine.circuit.view("inregion").unwrap().to_sorted_vec(),
        vec![(alice.clone(), alice.clone(), 1)]
    );
    assert_eq!(
        engine.circuit.view("result").unwrap().to_sorted_vec(),
        vec![(alice.clone(), money(5447), 1)]
    );
    assert_eq!(
        custspend(&engine),
        vec![(alice.clone(), money(5447), 1), (bob.clone(), money(12250), 1)],
        "dangling FK group survives (honest 6NF)"
    );

    // Idempotence: retracting bob again is a no-op (all his rows are gone).
    let result = engine.retract_entity(&bob);
    assert!(result.view_deltas.values().all(|d| d.is_empty()));
}

#[test]
fn interleaved_view_definitions_match_batch() {
    // Views added mid-stream: lineprice exists from the start (pure delta
    // path), custspend/inregion/result arrive after half the data (backfill),
    // then the rest of the data streams in (delta path through all four).
    let (entities, news, views) = fixture_lines();
    let typed = elaborate(SPEC12);
    let mut engine = Engine::new();
    let mut values = HashMap::new();

    let let_stmts: Vec<&TStmt> =
        typed.stmts.iter().filter(|s| matches!(s, TStmt::Let { .. })).collect();
    let new_stmts: Vec<&TStmt> =
        typed.stmts.iter().filter(|s| matches!(s, TStmt::New { .. })).collect();

    apply_stmt(&mut engine, let_stmts[0], &mut values); // lineprice
    for stmt in &new_stmts[..5] {
        apply_stmt(&mut engine, stmt, &mut values);
    }
    for stmt in &let_stmts[1..] {
        apply_stmt(&mut engine, stmt, &mut values); // custspend, inregion, result
    }
    for stmt in &new_stmts[5..] {
        apply_stmt(&mut engine, stmt, &mut values);
    }

    let oracle_src = format!(
        "{}\n{}\n{}\n",
        entities.join("\n"),
        news.join("\n"),
        views.join("\n"),
    );
    assert_views_match_batch(&engine, &oracle_src, "interleaved definitions");
}
