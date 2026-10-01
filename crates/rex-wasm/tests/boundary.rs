//! The wasm boundary, natively: everything a host can hand `RexApp`, and
//! everything it gets back, as the JSON that really crosses.
//!
//!  - **round trips**: a session's log and snapshots, serialized and fed to a
//!    fresh app, rebuild the same state — through the decoders in this crate,
//!    which nothing else tests;
//!  - **hostile calls**: any `dispatch`/`rebalance` a host can make either
//!    succeeds or returns an error and changes nothing, and never panics (a
//!    panic is an abort in wasm: the app is dead until the page reloads);
//!  - **damaged storage**: a snapshot or log that was truncated, edited, or
//!    written by something else is refused, not loaded.

use proptest::prelude::*;
use rex::types::shape_ir::{Encoding, ParamTy};
use rex_wasm::App;
use serde_json::{json, Value as Json};
use std::panic::{catch_unwind, AssertUnwindSafe};

fn example(path: &str) -> String {
    std::fs::read_to_string(format!("{}/../../examples/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn programs() -> Vec<(&'static str, String)> {
    vec![
        ("todomvc", example("todomvc/src/app.rex")),
        ("benchmark", example("js-framework-benchmark/src/app.rex")),
        ("kanban", example("kanban/src/board.rex")),
        ("chat", example("chat/src/app.rex")),
    ]
}

/// What a test needs to know about a program to make up calls to it: each
/// declared event's parameters, and each entity's sort number.
struct Schema {
    events: Vec<(String, Vec<(String, ParamTy)>)>,
    env: rex::types::Env,
}

impl Schema {
    fn of(src: &str) -> Schema {
        let checked = rex::check(&rex::parse(src).program);
        let events = checked
            .shapes
            .events
            .iter()
            .map(|e| (e.name.clone(), e.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect()))
            .collect();
        Schema { events, env: checked.env }
    }

    /// A well-typed JSON argument for a param, from a raw choice.
    fn arg(&self, ty: &ParamTy, n: usize) -> Json {
        match ty {
            ParamTy::Id(entity) => json!(format!("#{}:{}", self.env.entity_sort(entity).map(|s| s.0).unwrap_or(0), n % 9)),
            ParamTy::Scalar(Encoding::Text) => json!(TEXTS[n % TEXTS.len()]),
            ParamTy::Scalar(Encoding::Int) => json!(format!("i:{}", [0i64, 1, 3, 997, -2, i64::MAX][n % 6])),
            ParamTy::Scalar(Encoding::Money) => json!(format!("m:{}", n % 500)),
            ParamTy::Scalar(Encoding::Atom) => json!(ATOMS[n % ATOMS.len()]),
            ParamTy::Scalar(Encoding::Id) => json!("#0:0"),
            ParamTy::Rel(..) => {
                Json::Array((0..n % 5).map(|i| json!([format!("i:{i}"), TEXTS[(n + i) % TEXTS.len()], 1])).collect())
            }
        }
    }

    /// The JSON args for one made-up call of event `which`.
    fn call(&self, which: usize, n: usize) -> (&str, String) {
        let (name, params) = &self.events[which % self.events.len()];
        let args: serde_json::Map<String, Json> =
            params.iter().enumerate().map(|(i, (p, ty))| (p.clone(), self.arg(ty, n / (i + 1) + i))).collect();
        (name, Json::Object(args).to_string())
    }
}

const TEXTS: &[&str] =
    &["t:a", "t:", "t:a\\,b", "t:\\(x\\)", "t:\"q\"", "t:é😀", "t:\u{2028}", "t:\n", "t:a0", "t:a1"];
const ATOMS: &[&str] = &["@True", "@False", "@All", "@Active", "@Completed"];
// (chat has no atom-typed params; its ids are made up like every other program's.)

/// Boot `src` and run a made-up history against it.
fn drive(src: &str, picks: &[(usize, usize)]) -> App {
    let mut app = App::new(src).expect("the program checks");
    let schema = Schema::of(src);
    for (which, n) in picks {
        let (name, args) = schema.call(*which, *n);
        // Refusals (a toggle of a row that is gone) are fine; panics are not.
        let _ = app.dispatch(name, &args);
    }
    app
}

/// Everything observable about an app, as its own JSON.
fn observe(app: &App) -> (Json, Json) {
    (serde_json::from_str(&app.snapshot()).unwrap(), sorted_snapshot(&app.base_snapshot()))
}

/// A base snapshot with its tables and counters in a fixed order (the engine
/// writes them in hash order), and without empty tables — an empty table and
/// an absent one are the same thing.
fn sorted_snapshot(json: &str) -> Json {
    let mut v: Json = serde_json::from_str(json).unwrap();
    let key = |j: &Json| j.to_string();
    for field in ["nextId", "inputs"] {
        let items = v[field].as_array_mut().unwrap();
        for item in items.iter_mut() {
            if let Some(rows) = item.get_mut("rows").and_then(|r| r.as_array_mut()) {
                rows.sort_by_key(key);
            }
        }
        items.sort_by_key(key);
    }
    v["inputs"].as_array_mut().unwrap().retain(|i| !i["rows"].as_array().unwrap().is_empty());
    v
}

fn nodes(v: &Json) -> usize {
    1 + match v {
        Json::Array(a) => a.iter().map(nodes).sum(),
        Json::Object(o) => o.values().map(nodes).sum(),
        _ => 0,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn a_session_round_trips_through_its_own_json(
        which in 0usize..4,
        picks in prop::collection::vec((any::<usize>(), any::<usize>()), 0..24),
        cut in 0usize..24,
    ) {
        let (_, src) = &programs()[which];
        let live = drive(src, &picks);
        let want = observe(&live);

        // The whole log, replayed onto an empty app — silently and not.
        for silent in [true, false] {
            let mut replayed = App::for_restore(src).unwrap();
            replayed.replay(&live.log_since(0), silent).unwrap();
            prop_assert_eq!(&observe(&replayed), &want, "replay (silent: {})", silent);
            // Replay does not re-log: the host already holds the log.
            prop_assert_eq!(replayed.log_since(0), "[]");
        }

        // A snapshot taken part-way, then the rest of the log.
        let part = drive(src, &picks[..cut.min(picks.len())]);
        let snap = part.base_snapshot();
        let cursor = serde_json::from_str::<Json>(&snap).unwrap()["cursor"].as_u64().unwrap();
        let mut restored = App::for_restore(src).unwrap();
        restored.restore(&snap).unwrap();
        prop_assert_eq!(&observe(&restored), &observe(&part), "restore alone");
        restored.replay(&live.log_since(cursor), true).unwrap();
        prop_assert_eq!(&observe(&restored), &want, "snapshot at {} + tail", cursor);

        // A restored app keeps working, and numbers its next event after the
        // log it was restored from.
        let schema = Schema::of(src);
        let (name, args) = schema.call(0, 1);
        if restored.dispatch(name, &args).is_ok() {
            let tail: Json = serde_json::from_str(&restored.log_since(0)).unwrap();
            let full: Json = serde_json::from_str(&live.log_since(0)).unwrap();
            prop_assert_eq!(tail[0]["seq"].as_u64(), Some(full.as_array().unwrap().len() as u64));
        }
    }
}

/// `f` neither panics nor, if it fails, changes anything observable.
fn refuses_cleanly(app: &mut App, what: &str, f: impl FnOnce(&mut App) -> Result<String, String>) {
    let before = (observe(app), app.log_since(0));
    match catch_unwind(AssertUnwindSafe(|| f(app))) {
        Err(_) => panic!("{what}: panicked"),
        Ok(Err(_)) => assert_eq!((observe(app), app.log_since(0)), before, "{what}: failed, but changed state"),
        Ok(Ok(_)) => {}
    }
}

#[test]
fn hostile_dispatches_fail_cleanly() {
    let src = example("todomvc/src/app.rex");
    let mut app = drive(&src, &[(0, 0), (0, 1), (0, 2)]);
    let calls: Vec<(&str, String)> = vec![
        ("AddTodo", "".into()),
        ("AddTodo", "{".into()),
        ("AddTodo", "null".into()),
        ("AddTodo", "[]".into()),
        ("AddTodo", "42".into()),
        ("AddTodo", "\"text\"".into()),
        ("AddTodo", "{}".into()),
        ("AddTodo", r#"{"text":null}"#.into()),
        ("AddTodo", r#"{"text":5}"#.into()),
        ("AddTodo", r#"{"text":true}"#.into()),
        ("AddTodo", r#"{"text":{}}"#.into()),
        ("AddTodo", r#"{"text":"plain"}"#.into()),
        ("AddTodo", r#"{"text":"i:5"}"#.into()),
        ("AddTodo", r#"{"text":"t:a","extra":"t:b"}"#.into()),
        ("AddTodo", r#"{"text":"t:a","text":"t:b"}"#.into()),
        ("AddTodo", r#"{"text":[["i:0","t:a",1]]}"#.into()),
        ("AddTodo", format!(r#"{{"text":"t:{}"}}"#, "x".repeat(1_000_000))),
        ("", "{}".into()),
        ("NoSuchEvent", "{}".into()),
        ("@genesis", "{}".into()),
        ("@rebalance", r#"{"field":"t:text","rows":[]}"#.into()),
        ("local#TodoItem#editing#set", r##"{"t":"#1:0","#value":"@True"}"##.into()),
        ("ToggleTodo", r##"{"t":"#1:999"}"##.into()),
        ("ToggleTodo", r##"{"t":"#0:0"}"##.into()),
        ("ToggleTodo", r##"{"t":"#999:0"}"##.into()),
        ("ToggleTodo", r##"{"t":"#1:-1"}"##.into()),
        ("ToggleTodo", r##"{"t":"#1:18446744073709551616"}"##.into()),
        ("ToggleTodo", r##"{"t":"t:#1:0"}"##.into()),
        ("ToggleTodo", r##"{"t":"p(#1:0,#1:1)"}"##.into()),
        ("ToggleTodo", r#"{"t":"u"}"#.into()),
        ("EditTodo", r##"{"t":"#1:0"}"##.into()),
        ("EditTodo", r##"{"t":"#1:0","text":"@True"}"##.into()),
        ("SetFilter", r#"{"f":"@Nope"}"#.into()),
        ("SetFilter", r#"{"f":"@"}"#.into()),
        ("SetFilter", r#"{"f":"t:All"}"#.into()),
        ("ToggleAll", r#"{"done":"@All"}"#.into()),
        ("ToggleAll", r#"{"done":"i:1"}"#.into()),
    ];
    for (event, args) in calls {
        let shown: String = args.chars().take(60).collect();
        refuses_cleanly(&mut app, &format!("dispatch({event:?}, {shown})"), |a| a.dispatch(event, &args));
    }
    // After all that, the app still works.
    let before = observe(&app);
    app.dispatch("AddTodo", r#"{"text":"t:still here"}"#).unwrap();
    assert_ne!(observe(&app), before);
}

#[test]
fn hostile_relation_arguments_fail_cleanly() {
    let src = example("js-framework-benchmark/src/app.rex");
    let mut app = App::new(&src).unwrap();
    for labels in [
        "[]",
        "[[]]",
        r#"[["i:0"]]"#,
        r#"[["i:0","t:a"]]"#,
        r#"[["i:0","t:a",1,2,3]]"#,
        r#"[["i:0","t:a","1"]]"#,
        r#"[["i:0","t:a",1.5]]"#,
        r#"[["i:0","t:a",0]]"#,
        r#"[["i:0","t:a",-1]]"#,
        r#"[["i:0","t:a",1e30]]"#,
        r#"[["t:zero","t:a",1]]"#,
        r#"[["i:0","i:5",1]]"#,
        r#"[["i:0","t:a",1],["i:0","t:b",1]]"#,
        r#"[[5,"t:a",1]]"#,
        "[null]",
        r#"[["i:9223372036854775807","t:a",1]]"#,
        r#"[["i:-9223372036854775808","t:a",1]]"#,
        r#""t:not a relation""#,
        "{}",
    ] {
        refuses_cleanly(&mut app, &format!("Run with labels {labels}"), |a| {
            a.dispatch("Run", &format!(r#"{{"n":"i:3","labels":{labels}}}"#))
        });
        refuses_cleanly(&mut app, &format!("Add with labels {labels}"), |a| {
            a.dispatch("Add", &format!(r#"{{"labels":{labels}}}"#))
        });
    }
    serde_json::from_str::<Json>(&app.snapshot()).unwrap();
}

#[test]
fn hostile_rebalances_fail_cleanly() {
    let src = example("kanban/src/board.rex");
    let mut app = App::new(&src).unwrap();
    for (field, rows) in [
        ("pos", ""),
        ("pos", "{}"),
        ("pos", "[[]]"),
        ("pos", r##"[["#1:0"]]"##),
        ("pos", r##"[["#1:0","i:3"]]"##),
        ("pos", r#"[["t:x","t:a0"]]"#),
        ("pos", r##"[["#9:0","t:a0"]]"##),
        ("pos", r##"[["#1:99","t:a0"]]"##),
        ("nope", r##"[["#1:0","t:a0"]]"##),
        ("", r##"[["#1:0","t:a0"]]"##),
        ("list", r##"[["#1:0","#0:55"]]"##),
        ("list", r##"[["#1:0","#1:0"]]"##),
        ("pos", r##"[["#1:0","t:a"],["#1:0","t:b"]]"##),
    ] {
        refuses_cleanly(&mut app, &format!("rebalance({field:?}, {rows})"), |a| a.rebalance(field, rows));
    }
    assert!(app.read_view("no such view").is_err());
    assert!(app.bound_id("no such name").is_none());
}

#[test]
fn a_program_that_does_not_check_is_refused_with_its_diagnostics() {
    let deep = "(".repeat(100_000);
    for bad in ["entity", "entity A { x: }", "let y = A . .zz", "view main = div {", deep.as_str()] {
        match catch_unwind(|| App::new(bad).map(|_| ())) {
            Err(_) => panic!("booting {:?} panicked", &bad[..bad.len().min(30)]),
            Ok(Ok(())) => panic!("{:?} booted", &bad[..bad.len().min(30)]),
            Ok(Err(e)) => assert!(e.contains("error"), "{e}"),
        }
    }
    // The empty program is a program.
    assert!(App::new("").is_ok());
}

// --- damaged storage -----------------------------------------------------------------

/// Replace the `at`-th node of a JSON document (in walk order) with `junk`.
fn damage(v: &mut Json, at: &mut usize, junk: &Json) -> bool {
    if *at == 0 {
        *v = junk.clone();
        return true;
    }
    *at -= 1;
    match v {
        Json::Array(items) => items.iter_mut().any(|i| damage(i, at, junk)),
        Json::Object(map) => map.values_mut().any(|i| damage(i, at, junk)),
        _ => false,
    }
}

fn junk() -> Vec<Json> {
    vec![
        Json::Null,
        json!(true),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(1e300),
        json!(u64::MAX),
        json!(""),
        json!("x"),
        json!("#0:0"),
        json!("#99:0"),
        json!("i:1"),
        json!("t:"),
        json!("id:0"),
        json!("id:99"),
        json!("f:0:nope"),
        json!("f:99:text"),
        json!("f:x:y"),
        json!([]),
        json!({}),
        json!([[]]),
        json!([["#1:0", "#1:0", 1]]),
        json!([["#1:0", "#1:0", 2]]),
        json!([["#1:0", "t:a", -1]]),
        json!({"key": "id:1", "rows": [["#1:5", "#1:6", 1]]}),
    ]
}

#[test]
fn a_damaged_snapshot_is_refused_or_loads_consistently() {
    for (name, src) in programs() {
        let live = drive(&src, &[(0, 1), (1, 2), (2, 3), (0, 4), (3, 5), (4, 6), (0, 7)]);
        let good: Json = serde_json::from_str(&live.base_snapshot()).unwrap();
        let schema = Schema::of(&src);
        let (mut refused, mut loaded) = (0, 0);
        for at in 0..nodes(&good) {
            for j in junk() {
                let mut bad = good.clone();
                damage(&mut bad, &mut at.clone(), &j);
                if bad == good {
                    continue;
                }
                let text = bad.to_string();
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    let mut app = App::for_restore(&src).unwrap();
                    app.restore(&text)?;
                    // Whatever was accepted must be a state the app can run
                    // from: readable, snapshot-able again, and able to take
                    // every event without tripping over what it loaded.
                    serde_json::from_str::<Json>(&app.snapshot()).map_err(|e| e.to_string())?;
                    App::for_restore(&src).unwrap().restore(&app.base_snapshot())?;
                    for which in 0..schema.events.len() {
                        let (event, args) = schema.call(which, 1);
                        let _ = app.dispatch(event, &args);
                    }
                    Ok::<(), String>(())
                }));
                match outcome {
                    Err(_) => panic!("{name}: a snapshot with node {at} replaced by {j} panicked:\n{text}"),
                    Ok(Err(_)) => refused += 1,
                    Ok(Ok(())) => loaded += 1,
                }
            }
        }
        // Most damage must be caught; what loads is damage that is still a
        // well-formed state (a text changed to another text).
        assert!(refused > loaded, "{name}: {refused} refused, {loaded} loaded");
    }
}

#[test]
fn a_truncated_or_garbled_snapshot_is_refused() {
    let src = example("kanban/src/board.rex");
    let snap = App::new(&src).unwrap().base_snapshot();
    for cut in (0..snap.len()).step_by(7) {
        if !snap.is_char_boundary(cut) {
            continue;
        }
        let outcome = catch_unwind(|| App::for_restore(&src).unwrap().restore(&snap[..cut]));
        assert!(matches!(outcome, Ok(Err(_))), "a snapshot cut at {cut} was not refused");
    }
    for garbage in [
        "",
        "null",
        "[]",
        "{}",
        "0",
        "\"\"",
        r#"{"cursor":0}"#,
        r#"{"cursor":0,"nextId":[],"inputs":null}"#,
        r#"{"cursor":-1,"nextId":[],"inputs":[]}"#,
    ] {
        let outcome = catch_unwind(|| App::for_restore(&src).unwrap().restore(garbage));
        assert!(matches!(outcome, Ok(Err(_))), "{garbage:?} was not refused");
    }
}

#[test]
fn a_damaged_log_is_refused_and_never_panics() {
    for (name, src) in programs() {
        let live = drive(&src, &[(0, 1), (1, 2), (2, 3), (0, 4), (3, 5), (4, 6)]);
        let good: Json = serde_json::from_str(&live.log_since(0)).unwrap();
        for at in 0..nodes(&good) {
            for j in junk() {
                let mut bad = good.clone();
                damage(&mut bad, &mut at.clone(), &j);
                let text = bad.to_string();
                let outcome = catch_unwind(|| {
                    let mut app = App::for_restore(&src).unwrap();
                    let r = app.replay(&text, true);
                    // Replayed or refused, the app must still be readable.
                    serde_json::from_str::<Json>(&app.snapshot()).unwrap();
                    serde_json::from_str::<Json>(&app.base_snapshot()).unwrap();
                    r
                });
                assert!(outcome.is_ok(), "{name}: a log with node {at} replaced by {j} panicked:\n{text}");
            }
        }
    }
}
