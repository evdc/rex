//! S-21: every write to the engine is the record of an appended [`Event`];
//! replaying that log from empty reproduces the exact same live state.

use proptest::prelude::*;
use rex::dbsp::{Engine, Event};
use rex::eval::relation::BinaryRelation;
use rex::eval::Value;
use rex::events::{dispatch_event, replay};
use rex::types::shape_ir::EventDef;
use rex::types::typed::{TProgram, TStmt};
use rex::types::Env;
use std::collections::HashMap;

const SRC: &str = r#"
entity List { title: Text, pos: Text }
entity Card { title: Text, pos: Text, list: ListID }

let l0 = new List { title: "Todo",  pos: "a0" }
let l1 = new List { title: "Doing", pos: "a1" }
let c0 = new Card { title: "Design", pos: "a0", list: l0 }

let card_list  : Card -> ListID = .list
let card_pos   : Card -> Text = .pos
let card_title : Card -> Text = .title

event MoveCard(card: Card, list: List, pos: Text)
event AddList(title: Text, pos: Text)
event Rename(card: Card, title: Text)

on MoveCard(card, list, pos) => update card { list: list, pos: pos }
on AddList(title, pos)       => new List { title: title, pos: pos }
on Rename(card, title)       => card.title := title
"#;

struct App {
    engine: Engine,
    env: Env,
    events: Vec<EventDef>,
    values: HashMap<String, Value>,
}

impl App {
    fn dispatch(&mut self, name: &str, args: &[(&str, Value)]) -> Vec<Value> {
        let args: HashMap<String, rex::dbsp::ArgValue> =
            args.iter().map(|(k, v)| (k.to_string(), rex::dbsp::ArgValue::Value(v.clone()))).collect();
        dispatch_event(&mut self.engine, &self.env, &self.events, name, &args).unwrap().0
    }
}

fn checked_program() -> (TProgram, Env, Vec<EventDef>) {
    let parsed = rex::parse(SRC);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = rex::check(&parsed.program);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    (checked.elaborated.unwrap(), checked.env, checked.shapes.events)
}

fn setup() -> App {
    let (prog, env, events) = checked_program();
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &prog.stmts {
        engine.apply_typed_stmt(stmt, &mut values);
    }
    App { engine, env, events, values }
}

/// A fresh engine with the program's views registered but no base data — the
/// boot-time state a persisted snapshot's log replays onto (MVP-PLAN §2.2:
/// "views are backfilled on load").
fn boot_views() -> (Engine, Env, Vec<EventDef>) {
    let (prog, env, events) = checked_program();
    let mut engine = Engine::new();
    let mut values = HashMap::new();
    for stmt in &prog.stmts {
        if matches!(stmt, TStmt::New { .. }) {
            continue; // supplied by replaying the log's genesis events
        }
        engine.apply_typed_stmt(stmt, &mut values);
    }
    (engine, env, events)
}

fn text(s: &str) -> Value {
    Value::Text(rex::eval::intern(s))
}

/// Every view, and every base table, holds the same rows in both engines.
fn assert_engines_match(a: &Engine, b: &Engine) {
    for name in a.circuit.output_names() {
        let av: Vec<_> = a.circuit.view(name).unwrap().iter().collect();
        let bv: Vec<_> = b.circuit.view(name).unwrap().iter().collect();
        assert_eq!(av, bv, "view `{name}` diverged");
    }
    for key in a.circuit.input_keys() {
        let av: Vec<_> = a.circuit.input_integral(key).map(|r| r.iter().collect()).unwrap_or_default();
        let bv: Vec<_> = b.circuit.input_integral(key).map(|r| r.iter().collect()).unwrap_or_default();
        assert_eq!(av, bv, "base table {key:?} diverged");
    }
}

#[test]
fn program_setup_logs_genesis_events() {
    let app = setup();
    let log = app.engine.log();
    assert_eq!(log.len(), 3, "l0, l1, c0 each log one @genesis event");
    for e in log {
        assert_eq!(e.name, "@genesis");
    }
    assert_eq!(log[0].seq, 0);
    assert_eq!(log[2].seq, 2);
}

#[test]
fn dispatch_appends_a_named_event() {
    let mut app = setup();
    let c0 = app.values["c0"].clone();
    let l1 = app.values["l1"].clone();
    app.dispatch("MoveCard", &[("card", c0), ("list", l1), ("pos", text("a5"))]);
    let log = app.engine.log();
    assert_eq!(log.len(), 4, "3 genesis + 1 dispatched");
    let last = log.last().unwrap();
    assert_eq!(last.seq, 3);
    assert_eq!(last.name, "MoveCard");
    assert_eq!(last.args.len(), 3);
}

#[test]
fn replay_from_empty_reproduces_live_state() {
    let mut app = setup();
    let c0 = app.values["c0"].clone();
    let l0 = app.values["l0"].clone();
    let l1 = app.values["l1"].clone();

    app.dispatch("MoveCard", &[("card", c0.clone()), ("list", l1.clone()), ("pos", text("a5"))]);
    let new_ids = app.dispatch("AddList", &[("title", text("Done")), ("pos", text("a9"))]);
    let l2 = new_ids[0].clone();
    app.dispatch("MoveCard", &[("card", c0.clone()), ("list", l2), ("pos", text("a1"))]);
    app.dispatch("Rename", &[("card", c0), ("title", text("Ship it"))]);

    let (mut engine2, env2, events2) = boot_views();
    replay(&mut engine2, &env2, &events2, app.engine.log(), false).unwrap();

    assert_engines_match(&app.engine, &engine2);
    let _ = (l0, l1);
}

#[test]
fn silent_replay_matches_normal_replay() {
    let mut app = setup();
    let c0 = app.values["c0"].clone();
    let l1 = app.values["l1"].clone();
    app.dispatch("MoveCard", &[("card", c0.clone()), ("list", l1), ("pos", text("z"))]);
    app.dispatch("Rename", &[("card", c0), ("title", text("Renamed"))]);

    let (mut normal, env_n, events_n) = boot_views();
    replay(&mut normal, &env_n, &events_n, app.engine.log(), false).unwrap();

    let (mut silent, env_s, events_s) = boot_views();
    replay(&mut silent, &env_s, &events_s, app.engine.log(), true).unwrap();

    assert_engines_match(&normal, &silent);
}

#[derive(Debug, Clone)]
enum Op {
    AddList(String, String),
    Rename(String),
    Move(String),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        ("[a-zA-Z]{1,6}", "[a-c][0-9]?").prop_map(|(t, p)| Op::AddList(t, p)),
        "[a-zA-Z]{1,6}".prop_map(Op::Rename),
        "[a-c][0-9]?".prop_map(Op::Move),
    ]
}

/// Run a random op sequence against a live engine, returning it plus its log.
fn run_ops(ops: &[Op]) -> App {
    let mut app = setup();
    let mut lists = vec![app.values["l0"].clone(), app.values["l1"].clone()];
    let c0 = app.values["c0"].clone();
    let mut moves = 0usize;
    for op in ops {
        match op {
            Op::AddList(title, pos) => {
                let ids = app.dispatch("AddList", &[("title", text(title)), ("pos", text(pos))]);
                lists.push(ids[0].clone());
            }
            Op::Rename(title) => {
                app.dispatch("Rename", &[("card", c0.clone()), ("title", text(title))]);
            }
            Op::Move(pos) => {
                let list = lists[moves % lists.len()].clone();
                moves += 1;
                app.dispatch("MoveCard", &[("card", c0.clone()), ("list", list), ("pos", text(pos))]);
            }
        }
    }
    app
}

proptest! {
    /// Random event histories: replaying the log an arbitrary run produces,
    /// from an engine with only its views registered, reproduces the exact
    /// same base tables and views the live engine ended up with.
    #[test]
    fn replay_matches_live_for_any_history(ops in prop::collection::vec(op_strategy(), 0..16)) {
        let app = run_ops(&ops);
        let (mut engine2, env2, events2) = boot_views();
        replay(&mut engine2, &env2, &events2, app.engine.log(), false).unwrap();
        assert_engines_match(&app.engine, &engine2);
    }
}

/// Perf record (S-21 acceptance: "replay of a 10k-event log is within 2× of
/// live apply time"): `cargo test -p rex --release --test log -- --ignored
/// --nocapture replay_of_10k_events_is_within_2x_of_live_apply`.
#[test]
#[ignore]
fn replay_of_10k_events_is_within_2x_of_live_apply() {
    use std::time::Instant;

    let ops: Vec<Op> = (0..10_000)
        .map(|i| match i % 3 {
            0 => Op::AddList(format!("l{i}"), format!("{}", i % 26)),
            1 => Op::Rename(format!("r{i}")),
            _ => Op::Move(format!("{}", i % 26)),
        })
        .collect();

    let t0 = Instant::now();
    let app = run_ops(&ops);
    let live = t0.elapsed();

    let (mut engine2, env2, events2) = boot_views();
    let t1 = Instant::now();
    replay(&mut engine2, &env2, &events2, app.engine.log(), true).unwrap();
    let replayed = t1.elapsed();

    assert_engines_match(&app.engine, &engine2);
    println!("live apply: {live:?}, silent replay: {replayed:?}, ratio: {:.2}x", replayed.as_secs_f64() / live.as_secs_f64());
    assert!(
        replayed <= live * 2,
        "replay ({replayed:?}) exceeded 2x live apply ({live:?})"
    );
}

#[test]
fn events_carry_reserved_m4_fields_as_none() {
    let app = setup();
    for e in app.engine.log() {
        let Event { cause, intent, .. } = e;
        assert_eq!(*cause, None);
        assert_eq!(*intent, None);
    }
}

// --- S-22: cursor/log_since, base_snapshot/restore, @rebalance ---

#[test]
fn log_since_returns_a_seq_suffix() {
    let mut app = setup();
    let c0 = app.values["c0"].clone();
    let l1 = app.values["l1"].clone();
    app.dispatch("MoveCard", &[("card", c0.clone()), ("list", l1), ("pos", text("a5"))]);
    app.dispatch("Rename", &[("card", c0), ("title", text("Ship it"))]);

    assert_eq!(app.engine.log_since(0).count(), 5, "3 genesis + 2 dispatched");
    let tail: Vec<_> = app.engine.log_since(4).collect();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].name, "Rename");
    assert_eq!(app.engine.cursor(), 5);
}

#[test]
fn base_snapshot_then_restore_reproduces_live_state() {
    let mut app = setup();
    let c0 = app.values["c0"].clone();
    let l1 = app.values["l1"].clone();
    app.dispatch("MoveCard", &[("card", c0.clone()), ("list", l1), ("pos", text("a5"))]);
    app.dispatch("Rename", &[("card", c0), ("title", text("Ship it"))]);

    let snap = app.engine.base_snapshot();
    assert_eq!(snap.cursor, app.engine.cursor());

    let (mut restored, _env, _events) = boot_views();
    assert_eq!(restored.log().len(), 0);
    restored.restore(&snap);

    assert_engines_match(&app.engine, &restored);
    // restore doesn't append to the restoring engine's own log (S-22 — the
    // host already has the full history) but DOES fast-forward the seq
    // counter, so a later dispatch continues numbering without colliding.
    assert_eq!(restored.log().len(), 0);
    assert_eq!(restored.cursor(), app.engine.cursor());
}

#[test]
fn restore_then_dispatch_continues_seq_numbering() {
    let mut app = setup();
    let c0 = app.values["c0"].clone();
    app.dispatch("Rename", &[("card", c0.clone()), ("title", text("Ship it"))]);
    let cursor_before = app.engine.cursor();

    let (mut restored, env, events) = boot_views();
    restored.restore(&app.engine.base_snapshot());

    let mut bound = HashMap::new();
    bound.insert("card".to_string(), rex::dbsp::ArgValue::Value(c0));
    bound.insert("title".to_string(), rex::dbsp::ArgValue::Value(text("Again")));
    dispatch_event(&mut restored, &env, &events, "Rename", &bound).unwrap();

    assert_eq!(restored.log().len(), 1);
    assert_eq!(restored.log()[0].seq, cursor_before);
}

#[test]
fn rebalance_is_one_logged_event_covering_every_row() {
    let mut app = setup();
    let l0 = app.values["l0"].clone();
    let l1 = app.values["l1"].clone();
    let before = app.engine.log().len();

    app.engine.apply_rebalance("pos", &[(l0, text("a5")), (l1, text("b5"))]);

    let log = app.engine.log();
    assert_eq!(log.len(), before + 1, "one rebalance sweep, one logged event");
    assert_eq!(log.last().unwrap().name, "@rebalance");
}

#[test]
fn rebalance_replays_deterministically() {
    let mut app = setup();
    let l0 = app.values["l0"].clone();
    let l1 = app.values["l1"].clone();
    app.engine.apply_rebalance("pos", &[(l0, text("a5")), (l1, text("b5"))]);

    let (mut engine2, env2, events2) = boot_views();
    replay(&mut engine2, &env2, &events2, app.engine.log(), false).unwrap();

    assert_engines_match(&app.engine, &engine2);
}
