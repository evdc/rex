//! `let x = new E { … }` in a handler: `x` is the new row's id for the
//! statements after it.
//!
//! The id is predicted at dispatch (ids are per-sort and sequential) rather
//! than minted early, so these tests pin the prediction against what the
//! engine then mints, through every way a handler can create rows, and
//! through replay.

mod common;

use common::{check_base_invariant, check_replay, check_views, App};
use rex::dbsp::ArgValue;
use rex::eval::Value;
use std::collections::HashMap;

const SRC: &str = r#"
entity User { name: Text }
entity Msg { text: Text, sender: User }
entity Like { msg: Msg, user: User }
state current : User

event Seed()
event Pair(a: Text, b: Text)
event Bulk(names: Int -> Text, last: Text)
event Outer(name: Text)
event Inner(name: Text)
event Rewrite(name: Text)
event Undo(name: Text)
event Thread(text: Text)

on Seed() {
  let alice = new User { name: "Alice" }
  let bob   = new User { name: "Bob" }
  let m1 = new Msg { text: "hi", sender: alice }
  let m2 = new Msg { text: "yo", sender: bob }
  new Like { msg: m1, user: bob }
  new Like { msg: m2, user: alice }
  set current = alice
}
// Two rows of one sort: each name must mean its own row.
on Pair(a, b) {
  let x = new User { name: a }
  let y = new User { name: b }
  new Msg { text: a, sender: y }
  new Msg { text: b, sender: x }
}
// A bulk insert between two named rows of the same sort.
on Bulk(names, last) {
  let first = new User { name: "first" }
  new User from names as (i, n) { name: n }
  let tail = new User { name: last }
  new Msg { text: "first", sender: first }
  new Msg { text: "tail", sender: tail }
}
// A callee that creates rows, before and after the caller's own.
on Inner(name) {
  let u = new User { name: name }
  new Msg { text: "inner", sender: u }
}
on Outer(name) {
  let before = new User { name: "before" }
  do Inner(name)
  let after = new User { name: "after" }
  new Msg { text: "before", sender: before }
  new Msg { text: "after", sender: after }
  set current = after
}
// A named row as a target: writes compose with its creation.
on Rewrite(name) {
  let u = new User { name: "draft" }
  u.name := name
  new Msg { text: "by rewrite", sender: u }
}
on Undo(name) {
  let u = new User { name: name }
  let m = new Msg { text: "gone", sender: u }
  delete m
}
// A named row in a `where` predicate.
on Thread(text) {
  let m = new Msg { text: text, sender: current }
  new Like { msg: m, user: current }
  delete Like where .msg = m
  new Like { msg: m, user: current }
}

let author : Msg -> Text = .sender.name
let liked_by : Like -> Text = .user.name
let current_name : Unit -> Text = current . .name
"#;

fn text(s: &str) -> Value {
    Value::text(s)
}

/// `Msg.text -> sender's name`, through the join.
fn authors(app: &App) -> Vec<(String, String)> {
    let texts: HashMap<Value, Value> = app.field("Msg", "text").into_iter().collect();
    let mut out: Vec<(String, String)> = app
        .view("author")
        .into_iter()
        .map(|(m, name, _)| (texts[&m].to_string(), name.to_string()))
        .collect();
    out.sort();
    out
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut out: Vec<_> = v.iter().map(|(a, b)| (format!("{a:?}"), format!("{b:?}"))).collect();
    out.sort();
    out
}

#[test]
fn a_named_row_is_usable_as_a_field_value_and_a_state() {
    let mut app = App::build(SRC);
    let (ids, _) = app.dispatch("Seed", &[]).unwrap();
    // Minted in statement order: two users, two messages, two likes.
    assert_eq!(ids.len(), 6);
    assert_eq!(authors(&app), pairs(&[("hi", "Alice"), ("yo", "Bob")]));
    let mut likers: Vec<String> = app.view("liked_by").into_iter().map(|(_, n, _)| n.to_string()).collect();
    likers.sort();
    assert_eq!(likers, ["\"Alice\"", "\"Bob\""]);
    assert_eq!(app.view("current_name"), vec![(Value::Unit, text("Alice"), 1)]);
    // `current` holds the id the engine really minted for alice.
    assert_eq!(app.field("State#", "current")[0].1, ids[0]);
}

#[test]
fn two_named_rows_of_one_sort_are_not_confused() {
    let mut app = App::build(SRC);
    app.dispatch("Pair", &[("a", text("A")), ("b", text("B"))]).unwrap();
    assert_eq!(authors(&app), pairs(&[("A", "B"), ("B", "A")]));
    // …and again, now that the counters have moved.
    app.dispatch("Pair", &[("a", text("C")), ("b", text("D"))]).unwrap();
    assert_eq!(authors(&app), pairs(&[("A", "B"), ("B", "A"), ("C", "D"), ("D", "C")]));
}

#[test]
fn the_predicted_id_accounts_for_bulk_inserts_and_callees() {
    let mut app = App::build(SRC);
    let names = ArgValue::Rel((0..5).map(|i| (Value::Int(i), text(&format!("n{i}")), 1)).collect());
    let mut args = HashMap::from([("names".to_string(), names)]);
    args.insert("last".into(), ArgValue::Value(text("tail")));
    app.dispatch_args("Bulk", &args).unwrap();
    assert_eq!(authors(&app), pairs(&[("first", "first"), ("tail", "tail")]));
    assert_eq!(app.ids("User").len(), 7);

    app.dispatch("Outer", &[("name", text("mid"))]).unwrap();
    assert_eq!(
        authors(&app),
        pairs(&[("after", "after"), ("before", "before"), ("first", "first"), ("inner", "mid"), ("tail", "tail")])
    );
    assert_eq!(app.view("current_name"), vec![(Value::Unit, text("after"), 1)]);
    check_base_invariant(&app.engine).unwrap();
}

#[test]
fn a_named_row_can_be_written_and_deleted_in_the_handler_that_creates_it() {
    let mut app = App::build(SRC);
    app.dispatch("Rewrite", &[("name", text("final"))]).unwrap();
    assert_eq!(authors(&app), pairs(&[("by rewrite", "final")]));
    assert_eq!(app.field("User", "name").len(), 1);

    let users = app.ids("User").len();
    let (ids, _) = app.dispatch("Undo", &[("name", text("ghost"))]).unwrap();
    // Both were minted (ids are never reused), and the message is gone.
    assert_eq!(ids.len(), 2);
    assert_eq!(app.ids("User").len(), users + 1);
    assert_eq!(app.ids("Msg").len(), 1);
    check_base_invariant(&app.engine).unwrap();
    check_views(&app, "after Undo").unwrap();
}

#[test]
fn a_where_target_does_not_see_rows_this_handler_created() {
    let mut app = App::build(SRC);
    app.dispatch("Seed", &[]).unwrap();
    let likes = app.ids("Like").len();
    app.dispatch("Thread", &[("text", text("t"))]).unwrap();
    // Both likes are there. The id compares fine, but a `where` target is
    // read from the state before the event, like every other read — and the
    // first like was not in it. A handler deletes a row it created by name
    // (`delete m`), not by searching for it.
    let new_msg = app.ids("Msg").last().cloned().unwrap();
    let on_new = app.field("Like", "msg").into_iter().filter(|(_, m)| *m == new_msg).count();
    assert_eq!(on_new, 2);
    assert!(app.ids("Like").len() > likes);
    check_base_invariant(&app.engine).unwrap();
    check_views(&app, "after Thread").unwrap();
}

#[test]
fn replay_and_restore_reproduce_the_same_ids() {
    let mut app = App::build(SRC);
    app.dispatch("Seed", &[]).unwrap();
    app.dispatch("Pair", &[("a", text("A")), ("b", text("B"))]).unwrap();
    let snap = app.engine.base_snapshot();
    app.dispatch("Outer", &[("name", text("mid"))]).unwrap();
    app.dispatch("Undo", &[("name", text("ghost"))]).unwrap();
    app.dispatch("Thread", &[("text", text("t"))]).unwrap();
    check_views(&app, "live").unwrap();
    check_replay(SRC, &app).unwrap();
    common::check_restore(SRC, &app, &snap).unwrap();
}

#[test]
fn a_refused_handler_mints_nothing() {
    // `Thread` reads `current`, which has no value yet: the whole event is
    // refused, including the rows it would have named.
    let mut app = App::build(SRC);
    assert!(app.dispatch("Thread", &[("text", text("t"))]).is_err());
    assert!(app.ids("Msg").is_empty() && app.ids("Like").is_empty());
    // The next event gets the ids the refused one would have used.
    let (ids, _) = app.dispatch("Pair", &[("a", text("A")), ("b", text("B"))]).unwrap();
    assert_eq!(ids[2], Value::Id(app.env.entity_sort("Msg").unwrap(), 0));
}

fn error(body: &str) -> String {
    let src = format!("entity User {{ name: Text }}\nentity Msg {{ text: Text, sender: User }}\nevent E(u: User, rows: Int -> Text)\n{body}\n");
    common::check(&src).err().unwrap_or_else(|| panic!("accepted:\n{body}"))
}

#[test]
fn what_a_binding_may_not_do() {
    // Read from the row it names: reads see the state before the event.
    let e = error("on E(u, rows) {\n  let x = new User { name: \"a\" }\n  new Msg { text: x.name, sender: x }\n}");
    assert!(e.contains("was created by this handler"), "{e}");
    // Shadow a parameter, or an earlier binding.
    let e = error("on E(u, rows) {\n  let u = new User { name: \"a\" }\n}");
    assert!(e.contains("already a parameter or binding"), "{e}");
    let e = error("on E(u, rows) {\n  let x = new User { name: \"a\" }\n  let x = new User { name: \"b\" }\n}");
    assert!(e.contains("already a parameter or binding"), "{e}");
    // Name a bulk insert.
    let e = error("on E(u, rows) {\n  let x = new User from rows as (i, n) { name: n }\n}");
    assert!(e.contains("cannot be bound to one name"), "{e}");
    // Be used before it is bound, or as the wrong entity.
    let e = error("on E(u, rows) {\n  new Msg { text: \"t\", sender: x }\n  let x = new User { name: \"a\" }\n}");
    assert!(e.contains("unknown parameter `x`"), "{e}");
    let e = error("on E(u, rows) {\n  let m = new Msg { text: \"t\", sender: u }\n  new Msg { text: \"t\", sender: m }\n}");
    assert!(e.contains("expects") || e.contains("User"), "{e}");
    // Leak into another handler.
    let e = error("event F()\non E(u, rows) {\n  let x = new User { name: \"a\" }\n}\non F() => new Msg { text: \"t\", sender: x }");
    assert!(e.contains("unknown parameter `x`"), "{e}");
}
