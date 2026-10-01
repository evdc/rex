//! Entity keys: `entity E { …, key (f, g) }` — no two live rows agree on the
//! key's fields. A constraint on the state, enforced where state changes: a
//! transaction that would break it is rejected whole; a snapshot or a log
//! that breaks it is not loaded.

mod common;

use common::{check_base_invariant, check_replay, check_restore, check_views, App};
use proptest::prelude::*;
use rex::dbsp::{ArgValue, InputKey};
use rex::eval::Value;
use rex::events::{replay, restore, Refusal};
use std::collections::{BTreeMap, BTreeSet};

const SRC: &str = r#"
type Kind = Up | Down
entity User { name: Text, age: Int, key (name) }
entity Msg { text: Text }
entity Like { msg: Msg, user: User, key (msg, user) }
entity Vote { msg: Msg, user: User, kind: Kind, weight: Int, key (msg, user, kind) }

event Join(name: Text)
event Rename(u: User, age: Int)
event Post(text: Text)
event Like(m: Msg, u: User)
event Unlike(m: Msg, u: User)
event Twice(m: Msg, u: User)
event Relike(m: Msg, u: User)
event LikeThenDrop(m: Msg, u: User)
event Outer(m: Msg, u: User)
event Vote(m: Msg, u: User, kind: Kind, weight: Int)
event JoinAll(names: Int -> Text)
event Toggle(m: Msg, u: User)
event Tag(m: Msg, u: User)

let ada = new User { name: "Ada", age: 36 }
let m0  = new Msg { text: "hello" }
let _   = new Like { msg: m0, user: ada }

let likes     : Msg -> Int  = count(Like by .msg)
let liked     : Msg -> User = ~(Like . .msg) . .user
let by_name   : Text -> User = ~(User . .name)

on Join(name) => new User { name: name, age: 0 }
// A non-key field changes freely.
on Rename(u, age) => u.age := age
on Post(text) => new Msg { text: text }
on Like(m, u) => new Like { msg: m, user: u }
on Unlike(m, u) => delete Like where .msg = m & .user = u
// The same key twice in one transaction.
on Twice(m, u) {
  new Like { msg: m, user: u }
  new Like { msg: m, user: u }
}
// A row that is deleted gives its key up — to a statement after it.
on Relike(m, u) {
  delete Like where .msg = m & .user = u
  new Like { msg: m, user: u }
}
on LikeThenDrop(m, u) {
  new Like { msg: m, user: u }
  delete Like where .msg = m & .user = u
}
// A callee's rows are the caller's transaction.
on Outer(m, u) {
  new Msg { text: "outer" }
  do Like(m, u)
}
on Vote(m, u, kind, weight) => new Vote { msg: m, user: u, kind: kind, weight: weight }
on JoinAll(names) => new User from names as (i, name) { name: name, age: i }
// The constraint and an `if`: one like per pair, toggled.
on Toggle(m, u) {
  if (Like where .msg = m & .user = u) { delete Like where .msg = m & .user = u }
  else             { new Like { msg: m, user: u } }
}
// A named row of a keyed entity is used like any other.
on Tag(m, u) {
  let l = new Like { msg: m, user: u }
  new Vote { msg: m, user: u, kind: Up, weight: 1 }
  delete l
}
"#;

fn text(s: &str) -> Value {
    Value::text(s)
}

fn pair(app: &App, m: &str, u: &str) -> [(&'static str, Value); 2] {
    [("m", app.values[m].clone()), ("u", app.values[u].clone())]
}

/// Everything observable, for "a rejected event changed nothing".
fn fingerprint(app: &App) -> String {
    let mut views: Vec<_> = common::batch_views(app).into_iter().map(|(k, v)| (k, v.to_sorted_vec())).collect();
    views.sort();
    format!("{views:?} @ {}", app.engine.cursor())
}

fn likes(app: &App) -> usize {
    app.ids("Like").len()
}

fn ok(app: &App) {
    check_views(app, "views").unwrap();
    check_base_invariant(&app.engine).unwrap();
}

#[test]
fn a_second_row_with_the_same_key_rejects_the_event() {
    let mut app = App::build(SRC);
    let before = fingerprint(&app);
    // The seed row holds (m0, ada).
    assert_eq!(app.rejected("Like", &pair(&app, "m0", "ada")), "a `Like` with this `msg` and `user` already exists");
    assert_eq!(app.rejected("Join", &[("name", text("Ada"))]), "a `User` with this `name` already exists");
    assert_eq!(fingerprint(&app), before);
    // A key that differs in either part is free.
    let (ids, _) = app.dispatch("Join", &[("name", text("Bo"))]).unwrap();
    app.values.insert("bo".into(), ids[0].clone());
    let (ids, _) = app.dispatch("Post", &[("text", text("two"))]).unwrap();
    app.values.insert("m1".into(), ids[0].clone());
    app.dispatch("Like", &pair(&app, "m0", "bo")).unwrap();
    app.dispatch("Like", &pair(&app, "m1", "ada")).unwrap();
    app.dispatch("Like", &pair(&app, "m1", "bo")).unwrap();
    assert_eq!(likes(&app), 4);
    for (m, u) in [("m0", "ada"), ("m0", "bo"), ("m1", "ada"), ("m1", "bo")] {
        app.rejected("Like", &pair(&app, m, u));
    }
    // Keys are compared as values: text is text, whatever it looks like.
    app.dispatch("Join", &[("name", text("ada"))]).unwrap();
    app.dispatch("Join", &[("name", text(""))]).unwrap();
    app.rejected("Join", &[("name", text(""))]);
    ok(&app);
    check_replay(SRC, &app).unwrap();
}

#[test]
fn a_deleted_row_gives_its_key_up() {
    let mut app = App::build(SRC);
    app.dispatch("Unlike", &pair(&app, "m0", "ada")).unwrap();
    assert_eq!(likes(&app), 0);
    // A later event may take the key: a new row, with a new id.
    let (ids, _) = app.dispatch("Like", &pair(&app, "m0", "ada")).unwrap();
    assert_eq!(ids, [Value::Id(app.env.entity_sort("Like").unwrap(), 1)]);
    app.rejected("Like", &pair(&app, "m0", "ada"));
    // …and so may a later statement of the same event.
    let (ids, _) = app.dispatch("Relike", &pair(&app, "m0", "ada")).unwrap();
    assert_eq!((likes(&app), ids.len()), (1, 1));
    // The other order asks for the key while it is still held.
    let before = fingerprint(&app);
    app.rejected("LikeThenDrop", &pair(&app, "m0", "ada"));
    assert_eq!(fingerprint(&app), before);
    ok(&app);
    check_replay(SRC, &app).unwrap();
}

#[test]
fn a_transaction_is_checked_against_its_own_rows() {
    let mut app = App::build(SRC);
    let (ids, _) = app.dispatch("Join", &[("name", text("Bo"))]).unwrap();
    app.values.insert("bo".into(), ids[0].clone());
    let before = fingerprint(&app);
    // Neither row exists yet; the second collides with the first.
    assert_eq!(app.rejected("Twice", &pair(&app, "m0", "bo")), "a `Like` with this `msg` and `user` already exists");
    // A callee's collision rejects the caller: its `Msg` is not kept.
    app.rejected("Outer", &pair(&app, "m0", "ada"));
    assert_eq!(fingerprint(&app), before);
    app.dispatch("Outer", &pair(&app, "m0", "bo")).unwrap();
    assert_eq!((likes(&app), app.ids("Msg").len()), (2, 2));
    // Bulk rows: all or none.
    let names = |v: &[&str]| -> HashMapArgs {
        let rows = v.iter().enumerate().map(|(i, n)| (Value::Int(i as i64), text(n), 1)).collect();
        [("names".to_string(), ArgValue::Rel(rows))].into()
    };
    let users = app.ids("User").len();
    assert!(matches!(app.try_dispatch("JoinAll", &names(&["Cy", "Di", "Cy"])), Err(Refusal::Rejected(_))));
    assert!(matches!(app.try_dispatch("JoinAll", &names(&["Cy", "Ada"])), Err(Refusal::Rejected(_))));
    assert_eq!(app.ids("User").len(), users);
    app.try_dispatch("JoinAll", &names(&["Cy", "Di"])).unwrap();
    assert_eq!(app.ids("User").len(), users + 2);
    ok(&app);
    check_replay(SRC, &app).unwrap();
}

type HashMapArgs = std::collections::HashMap<String, ArgValue>;

#[test]
fn a_key_of_three_fields_and_a_field_outside_it() {
    let mut app = App::build(SRC);
    let vote = |app: &mut App, kind: &str, w: i64| {
        let args = [("m", app.values["m0"].clone()), ("u", app.values["ada"].clone()), ("kind", Value::atom(kind)), ("weight", Value::Int(w))];
        app.try_dispatch("Vote", &args.into_iter().map(|(k, v)| (k.to_string(), ArgValue::Value(v))).collect()).map(|_| ())
    };
    vote(&mut app, "Up", 1).unwrap();
    // `kind` is part of the key: a different kind is a different row.
    vote(&mut app, "Down", 1).unwrap();
    // `weight` is not: it does not make the key new.
    assert_eq!(vote(&mut app, "Up", 5), Err(Refusal::Rejected("a `Vote` with this `msg` and `user` and `kind` already exists".into())));
    assert_eq!(app.ids("Vote").len(), 2);
    // A non-key field of a keyed row is written like any other.
    app.dispatch("Rename", &[("u", app.values["ada"].clone()), ("age", Value::Int(37))]).unwrap();
    assert_eq!(app.field("User", "age")[0].1, Value::Int(37));
    app.rejected("Join", &[("name", text("Ada"))]);
    ok(&app);
}

#[test]
fn a_named_row_of_a_keyed_entity_and_a_toggle() {
    let mut app = App::build(SRC);
    for want in [0, 1, 0, 1] {
        app.dispatch("Toggle", &pair(&app, "m0", "ada")).unwrap();
        assert_eq!(likes(&app), want);
    }
    // `Tag` creates a like and deletes it again by name: but the key is held.
    app.rejected("Tag", &pair(&app, "m0", "ada"));
    app.dispatch("Toggle", &pair(&app, "m0", "ada")).unwrap();
    app.dispatch("Tag", &pair(&app, "m0", "ada")).unwrap();
    assert_eq!((likes(&app), app.ids("Vote").len()), (0, 1));
    ok(&app);
    check_replay(SRC, &app).unwrap();
}

#[test]
fn the_index_is_a_view_and_survives_restore() {
    let mut app = App::build(SRC);
    app.dispatch("Join", &[("name", text("Bo"))]).unwrap();
    let snap = app.engine.base_snapshot();
    app.dispatch("Join", &[("name", text("Cy"))]).unwrap();
    assert_eq!(app.view("by_name").len(), 3);
    assert_eq!(app.view("key#User"), app.view("by_name"));
    check_restore(SRC, &app, &snap).unwrap();
    // A restored engine still enforces it.
    let mut fresh = App::build_empty(SRC);
    restore(&mut fresh.engine, &fresh.env, &app.engine.base_snapshot()).unwrap();
    fresh.rejected("Join", &[("name", text("Cy"))]);
    fresh.dispatch("Join", &[("name", text("Di"))]).unwrap();
}

#[test]
fn a_snapshot_or_a_log_that_breaks_a_key_is_not_loaded() {
    let mut app = App::build(SRC);
    app.dispatch("Join", &[("name", text("Bo"))]).unwrap();
    let user = app.env.entity_sort("User").unwrap();
    let name = InputKey::Field(user, rex::eval::intern("name"));
    let table = |snap: &rex::dbsp::BaseSnapshot, key: InputKey| snap.inputs.iter().position(|(k, _)| *k == key).unwrap();
    let load = |snap: &rex::dbsp::BaseSnapshot| {
        let mut fresh = App::build_empty(SRC);
        let res = restore(&mut fresh.engine, &fresh.env, snap);
        assert!(res.is_ok() || fresh.engine.cursor() == 0, "refused, but something was loaded");
        res
    };
    load(&app.engine.base_snapshot()).unwrap();
    // Two users with one name.
    let mut snap = app.engine.base_snapshot();
    let t = table(&snap, name);
    for row in &mut snap.inputs[t].1 {
        row.1 = text("Ada");
    }
    let err = load(&snap).unwrap_err();
    assert!(err.contains("has no key of its own"), "{err}");
    // A user with no name at all.
    let mut snap = app.engine.base_snapshot();
    let t = table(&snap, name);
    snap.inputs[t].1.pop();
    assert!(load(&snap).is_err());

    // A log whose seed rows collide.
    let mut log = app.engine.log().to_vec();
    let seed_like = log.iter().position(|e| e.args.iter().any(|(k, _)| k == "msg")).unwrap();
    let mut again = log[seed_like].clone();
    again.seq = log.len() as u64;
    log.push(again);
    let mut fresh = App::build_empty(SRC);
    let err = replay(&mut fresh.engine, &fresh.env, &fresh.events, &log, true).unwrap_err();
    assert!(err.contains("already exists"), "{err}");
}

// --- what the checker says ----------------------------------------------------------------

fn error(src: &str) -> String {
    common::check(src).err().unwrap_or_else(|| panic!("accepted:\n{src}"))
}

#[test]
fn what_a_key_may_not_be() {
    let e = error("entity T { a: Int, key (b) }\n");
    assert!(e.contains("`key` names `b`, which is not a field of `T`"), "{e}");
    let e = error("entity T { a: Int, key (a, a) }\n");
    assert!(e.contains("names `a` twice"), "{e}");
    let e = error("entity T { a: Int, b: Int, key (a), key (b) }\n");
    assert!(e.contains("an entity has one `key`"), "{e}");
    let e = error("entity T { a: Int, key () }\n");
    assert!(e.contains("a field name in `key (…)`"), "{e}");
    let head = "entity T { a: Int, b: Int, c: Int, key (a, b) }\nevent E(t: T, n: Int)\n";
    // Every row has its whole key.
    let e = error(&format!("{head}on E(t, n) => new T {{ a: n, c: n }}\n"));
    assert!(e.contains("must give `b`: it is part of the entity's key"), "{e}");
    let e = error(&format!("{head}let t0 = new T {{ b: 1 }}\n"));
    assert!(e.contains("must give `a`"), "{e}");
    // And keeps it.
    let e = error(&format!("{head}on E(t, n) => t.a := n\n"));
    assert!(e.contains("`a` is part of the key of `T` and cannot be changed"), "{e}");
    let e = error(&format!("{head}on E(t, n) => update T where .c = n {{ c: 0, b: n }}\n"));
    assert!(e.contains("`b` is part of the key of `T`"), "{e}");
    common::check(&format!("{head}on E(t, n) => t.c := n\n")).unwrap();
    // Seed rows are checked before anything runs.
    let e = error(&format!("{head}let x = new T {{ a: 1, b: 2, c: 3 }}\nlet y = new T {{ a: 1, b: 2, c: 4 }}\n"));
    assert!(e.contains("another seed `T` already has this `a`, `b`"), "{e}");
    common::check(&format!("{head}let x = new T {{ a: 1, b: 2 }}\nlet y = new T {{ a: 2, b: 1 }}\n")).unwrap();
    let e = error("entity U { name: Text }\nentity L { u: U, key (u) }\nlet a = new U { name: \"a\" }\nlet x = new L { u: a }\nlet y = new L { u: a }\n");
    assert!(e.contains("another seed `L`"), "{e}");
}

#[test]
fn key_is_a_word_only_in_front_of_a_parenthesis() {
    // A field, a param and a binder may all be called `key`.
    let src = "entity T { key: Int, name: Text, key (key) }\nevent E(key: Int)\non E(key) => new T { key: key, name: \"n\" }\nlet ks : T -> Int = .key\n";
    let mut app = App::build(src);
    app.dispatch("E", &[("key", Value::Int(1))]).unwrap();
    app.rejected("E", &[("key", Value::Int(1))]);
    let parsed = rex::parse(src);
    assert!(rex::program_to_sexpr(&parsed.program).contains("(key key)"));
}

// --- a model: a set of pairs ----------------------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    Join(u8),
    Post,
    Like(usize, usize),
    Unlike(usize, usize),
    Toggle(usize, usize),
    Twice(usize, usize),
    Relike(usize, usize),
}

fn op() -> impl Strategy<Value = Op> {
    let p = || (0usize..4, 0usize..4);
    prop_oneof![
        2 => (0u8..5).prop_map(Op::Join),
        1 => Just(Op::Post),
        5 => p().prop_map(|(m, u)| Op::Like(m, u)),
        3 => p().prop_map(|(m, u)| Op::Unlike(m, u)),
        3 => p().prop_map(|(m, u)| Op::Toggle(m, u)),
        1 => p().prop_map(|(m, u)| Op::Twice(m, u)),
        2 => p().prop_map(|(m, u)| Op::Relike(m, u)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: common::cases(150), ..ProptestConfig::default() })]

    #[test]
    fn keyed_rows_are_a_set(ops in prop::collection::vec(op(), 0..50)) {
        let mut app = App::build(SRC);
        let mut users: BTreeMap<String, Value> = [("Ada".to_string(), app.values["ada"].clone())].into();
        let mut msgs = vec![app.values["m0"].clone()];
        let mut set: BTreeSet<(Value, Value)> = [(msgs[0].clone(), users["Ada"].clone())].into();
        for op in &ops {
            let user_ids: Vec<Value> = users.values().cloned().collect();
            let at = |m: usize, u: usize| (msgs[m % msgs.len()].clone(), user_ids[u % user_ids.len()].clone());
            let args = |p: &(Value, Value)| [("m", p.0.clone()), ("u", p.1.clone())];
            let before = fingerprint(&app);
            // What the model says must happen: `Some(accepted)`.
            let (name, call, accepted): (&str, Vec<(&str, Value)>, bool) = match op {
                Op::Join(n) => {
                    let name = format!("u{n}");
                    let fresh = !users.contains_key(&name);
                    ("Join", vec![("name", text(&name))], fresh)
                }
                Op::Post => ("Post", vec![("text", text("t"))], true),
                Op::Like(m, u) => { let p = at(*m, *u); ("Like", args(&p).to_vec(), !set.contains(&p)) }
                Op::Unlike(m, u) => { let p = at(*m, *u); ("Unlike", args(&p).to_vec(), true) }
                Op::Toggle(m, u) => { let p = at(*m, *u); ("Toggle", args(&p).to_vec(), true) }
                Op::Twice(m, u) => { let p = at(*m, *u); ("Twice", args(&p).to_vec(), false) }
                Op::Relike(m, u) => { let p = at(*m, *u); ("Relike", args(&p).to_vec(), true) }
            };
            let got = app.dispatch(name, &call);
            prop_assert_eq!(got.is_ok(), accepted, "{:?}: {:?}", op, got.as_ref().err());
            match (op, got) {
                (_, Err(_)) => prop_assert_eq!(fingerprint(&app), before, "{:?} was rejected but changed something", op),
                (Op::Join(n), Ok((ids, _))) => { users.insert(format!("u{n}"), ids[0].clone()); }
                (Op::Post, Ok((ids, _))) => msgs.push(ids[0].clone()),
                (Op::Like(m, u) | Op::Relike(m, u), Ok(_)) => { set.insert(at(*m, *u)); }
                (Op::Unlike(m, u), Ok(_)) => { set.remove(&at(*m, *u)); }
                (Op::Toggle(m, u), Ok(_)) => { let p = at(*m, *u); if !set.remove(&p) { set.insert(p); } }
                (Op::Twice(..), Ok(_)) => unreachable!(),
            }
            // The rows are exactly the set: one row per pair.
            let rows: Vec<(Value, Value)> = {
                let user: BTreeMap<Value, Value> = app.field("Like", "user").into_iter().collect();
                app.field("Like", "msg").into_iter().map(|(id, m)| (m, user[&id].clone())).collect()
            };
            prop_assert_eq!(rows.len(), set.len(), "after {:?}", op);
            prop_assert_eq!(rows.into_iter().collect::<BTreeSet<_>>(), set.clone(), "after {:?}", op);
            check_base_invariant(&app.engine).map_err(TestCaseError::fail)?;
        }
        check_views(&app, "at the end").map_err(TestCaseError::fail)?;
        check_replay(SRC, &app).map_err(TestCaseError::fail)?;
    }
}
