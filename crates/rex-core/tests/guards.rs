//! Handler guards (`on E(p) where (cond) else "reason" { … }`) and inline
//! `if (cond) { … } else { … }`.
//!
//! A guard is a precondition: if it does not hold in the state before the
//! event, the event is **rejected** — nothing written, nothing logged. An
//! `if` chooses what an accepted event does. Both read the pre-event state,
//! and both are written in the query language, as a filter at the handler's
//! params.

mod common;

use common::{check_base_invariant, check_replay, check_views, App};
use proptest::prelude::*;
use rex::dbsp::ArgValue;
use rex::eval::Value;
use rex::events::Refusal;
use rex::types::shape_ir::{Cond, MutationIR};
use std::collections::HashMap;

fn text(s: &str) -> Value {
    Value::text(s)
}

fn id(app: &App, entity: &str, n: u64) -> Value {
    Value::Id(app.env.entity_sort(entity).unwrap(), n)
}

/// How each conjunct of an event's guard is tested: `unit` (a view at the
/// Unit point), `key` (a keyset view, tested at an argument) or `eval`.
fn tiers(app: &App, event: &str) -> Vec<&'static str> {
    // A guard is an `if` around the body whose `else` rejects.
    match app.event(event).body.as_slice() {
        [MutationIR::If { conds, els, .. }] if matches!(els.as_slice(), [MutationIR::Reject { .. }]) => conds
            .iter()
            .map(|c| match c {
                Cond::Holds { .. } => "unit",
                Cond::HoldsAt { .. } => "key",
                Cond::Eval(_) => "eval",
                Cond::Exists { .. } => "rows",
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Everything observable, for "a rejected event changed nothing".
fn fingerprint(app: &App) -> String {
    let mut views: Vec<_> = common::batch_views(app).into_iter().map(|(k, v)| (k, v.to_sorted_vec())).collect();
    views.sort();
    format!("{views:?} @ {}", app.engine.cursor())
}

const NOTES: &str = r#"
entity User { name: Text }
entity Note { text: Text, owner: User, locked: Bool, stars: Int }
state current : User
state frozen : Bool = False

event Seed()
event Login(u: User)
event Logout()
event Add(text: Text)
event Lock(n: Note)
event Edit(n: Note, text: Text)
event Star(n: Note, d: Int)
event Rename(u: User, name: Text)
event Freeze(v: Bool)
event AddTwice(text: Text)
event Poke(n: Note, k: Int)

let users   : Unit -> Int = count(User by unit)
let notes_of : User -> Int = count(Note by .owner)

// No param: a view at the Unit point.
on Seed() where (users = 0) else "already seeded" {
  let a = new User { name: "Ada" }
  new User { name: "Bo" }
  set current = a
}
on Login(u) where (not current) else "someone is logged in" => set current = u
on Logout() where (current) => delete User where .name = "nobody"
// A state, a scalar argument, and a view read at the current user.
on Add(text) where (current & not frozen & text != "" & not current.notes_of >= 3) else "cannot add" =>
  new Note { text: text, owner: current, locked: False, stars: 0 }
// One entity param: a keyset view.
on Lock(n) where (not n.locked) else "already locked" => n.locked := True
on Edit(n, text) where (not n.locked & n.owner = current & text != n.text) else "cannot edit" => n.text := text
// Two things in one conjunct: evaluated at dispatch.
on Star(n, d) where (d > 0 | n.stars + d >= 0) => n.stars := n.stars + d
on Rename(u, name) where (u & name != "") => u.name := name
on Freeze(v) => set frozen = v
// Only the guard reads the row; the body would run without it.
on Poke(n, k) where (n.stars >= k) else "not enough stars" => set frozen = True
// `do` into a guarded handler: its guard is the caller's too.
on AddTwice(text) {
  do Add(text)
  do Add(text ++ "!")
}
"#;

fn notes() -> App {
    let mut app = App::build(NOTES);
    app.dispatch("Seed", &[]).unwrap();
    app
}

#[test]
fn each_conjunct_is_tested_the_cheapest_way_it_can_be() {
    let app = App::build(NOTES);
    assert_eq!(tiers(&app, "Seed"), ["unit"]);
    assert_eq!(tiers(&app, "Login"), ["unit"]);
    // current, not frozen: Unit views. text != "": the argument alone.
    // not current.notes_of >= 3: no param at all, so a Unit view too.
    assert_eq!(tiers(&app, "Add"), ["unit", "unit", "eval", "unit"]);
    assert_eq!(tiers(&app, "Lock"), ["key"]);
    // not n.locked, n.owner = current: keyset views over Note. The third
    // names two params.
    assert_eq!(tiers(&app, "Edit"), ["key", "key", "eval"]);
    assert_eq!(tiers(&app, "Star"), ["eval"]);
    assert_eq!(tiers(&app, "Rename"), ["key", "eval"]);
    assert!(tiers(&app, "Freeze").is_empty());
    // A row test is a target: a keyset view when it names no param, a scan
    // when it does.
    let app = App::build(ROWS);
    assert_eq!(tiers(&app, "AddOnce"), ["rows"]);
    assert_eq!(tiers(&app, "Mine"), ["unit", "rows", "unit"]);
}

#[test]
fn a_rejected_event_writes_nothing_logs_nothing_and_says_why() {
    let mut app = notes();
    let before = fingerprint(&app);
    assert_eq!(app.rejected("Seed", &[]), "already seeded");
    assert_eq!(app.rejected("Login", &[("u", id(&app, "User", 1))]), "someone is logged in");
    // No `else`: a default reason naming the event.
    assert_eq!(app.rejected("Add", &[("text", text(""))]), "cannot add");
    assert_eq!(app.rejected("Rename", &[("u", id(&app, "User", 0)), ("name", text(""))]), "the guard of `Rename` does not hold");
    assert_eq!(fingerprint(&app), before);
    check_views(&app, "after rejections").unwrap();
}

#[test]
fn a_guard_reads_the_state_before_the_event() {
    let mut app = App::build(NOTES);
    // `Seed` creates the users its own guard counts: the guard saw none.
    app.dispatch("Seed", &[]).unwrap();
    assert_eq!(app.ids("User").len(), 2);
    assert_eq!(app.rejected("Seed", &[]), "already seeded");
}

#[test]
fn a_unit_guard_follows_the_state_it_reads() {
    let mut app = notes();
    for i in 0..3 {
        app.dispatch("Add", &[("text", text(&format!("n{i}")))]).unwrap();
    }
    // The fourth is over the per-user limit.
    assert_eq!(app.rejected("Add", &[("text", text("n3"))]), "cannot add");
    app.dispatch("Freeze", &[("v", Value::atom("True"))]).unwrap();
    app.dispatch("Freeze", &[("v", Value::atom("False"))]).unwrap();
    // `where (current)`: a state that has a value.
    app.dispatch("Logout", &[]).unwrap();
    let bo = id(&app, "User", 1);
    assert_eq!(app.rejected("Login", &[("u", bo)]), "someone is logged in");
    assert_eq!(app.ids("Note").len(), 3);
}

#[test]
fn at_most_n_can_be_written_either_way() {
    // A user with no notes has a count of 0 — an entity's rows are group
    // keys whether or not anything is in the group — so `notes_of < 3`
    // admits their first note just as `not notes_of >= 3` does.
    for src in [NOTES.to_string(), NOTES.replace("not current.notes_of >= 3", "current.notes_of < 3")] {
        let mut app = App::build(&src);
        app.dispatch("Seed", &[]).unwrap();
        for i in 0..3 {
            app.dispatch("Add", &[("text", text(&format!("n{i}")))]).unwrap();
        }
        assert_eq!(app.rejected("Add", &[("text", text("n3"))]), "cannot add");
    }
}

#[test]
fn a_keyed_guard_is_tested_at_the_argument() {
    let mut app = notes();
    let (ids, _) = app.dispatch("Add", &[("text", text("a"))]).unwrap();
    let (n, ghost) = (ids[0].clone(), id(&app, "Note", 99));
    app.dispatch("Lock", &[("n", n.clone())]).unwrap();
    assert_eq!(app.rejected("Lock", &[("n", n.clone())]), "already locked");
    // A row that does not exist is in no keyset: rejected, not an error.
    assert_eq!(app.rejected("Lock", &[("n", ghost)]), "already locked");
    // Locked, so no edit; and never someone else's note.
    assert_eq!(app.rejected("Edit", &[("n", n), ("text", text("b"))]), "cannot edit");
}

#[test]
fn conjuncts_combine_and_an_evaluated_one_treats_absence_as_false() {
    let mut app = notes();
    let (ids, _) = app.dispatch("Add", &[("text", text("a"))]).unwrap();
    let n = ids[0].clone();
    // text != n.text
    assert_eq!(app.rejected("Edit", &[("n", n.clone()), ("text", text("a"))]), "cannot edit");
    app.dispatch("Edit", &[("n", n.clone()), ("text", text("b"))]).unwrap();
    // `|`: by > 0, or the result stays non-negative.
    app.dispatch("Star", &[("n", n.clone()), ("d", Value::Int(2))]).unwrap();
    app.dispatch("Star", &[("n", n.clone()), ("d", Value::Int(-2))]).unwrap();
    app.rejected("Star", &[("n", n.clone()), ("d", Value::Int(-1))]);
    // The second disjunct reads a row that is not there: false, so rejected.
    app.rejected("Star", &[("n", id(&app, "Note", 99)), ("d", Value::Int(-1))]);
    assert_eq!(app.field("Note", "stars")[0].1, Value::Int(0));
    // A condition that reads a row that is not there is false — even when
    // nothing in the body would have noticed.
    assert_eq!(app.rejected("Poke", &[("n", id(&app, "Note", 99)), ("k", Value::Int(-5))]), "not enough stars");
    assert_eq!(app.field("State#", "frozen")[0].1, Value::atom("False"));
    app.dispatch("Poke", &[("n", n.clone()), ("k", Value::Int(0))]).unwrap();
    app.dispatch("Freeze", &[("v", Value::atom("False"))]).unwrap();
    // `where (u)`: the row must exist.
    app.rejected("Rename", &[("u", id(&app, "User", 9)), ("name", text("x"))]);
    app.dispatch("Rename", &[("u", id(&app, "User", 1)), ("name", text("Bea"))]).unwrap();
}

#[test]
fn a_guard_of_a_callee_rejects_the_whole_event() {
    let mut app = notes();
    app.dispatch("AddTwice", &[("text", text("x"))]).unwrap();
    assert_eq!(app.ids("Note").len(), 2);
    // Two more would make four: the second `do` is over the limit, and its
    // guard — read before the event, when there were two — still holds, so
    // both go in. The limit bites on the next one.
    app.dispatch("AddTwice", &[("text", text("y"))]).unwrap();
    assert_eq!(app.ids("Note").len(), 4);
    let before = fingerprint(&app);
    assert_eq!(app.rejected("AddTwice", &[("text", text("z"))]), "cannot add");
    // An empty text fails the first callee's guard; the second's would pass
    // ("!" is not empty), but nothing of the event happens.
    let mut fresh = notes();
    assert_eq!(fresh.rejected("AddTwice", &[("text", text(""))]), "cannot add");
    assert!(fresh.ids("Note").is_empty());
    assert_eq!(fingerprint(&app), before);
}

#[test]
fn invalid_calls_are_not_rejections() {
    let mut app = notes();
    let call = |app: &mut App, name: &str, args: &[(&str, Value)]| {
        let args: HashMap<String, ArgValue> = args.iter().map(|(k, v)| (k.to_string(), ArgValue::Value(v.clone()))).collect();
        app.try_dispatch(name, &args).map(|_| ())
    };
    assert!(matches!(call(&mut app, "Nope", &[]), Err(Refusal::Invalid(_))));
    assert!(matches!(call(&mut app, "Add", &[]), Err(Refusal::Invalid(_))));
    assert!(matches!(call(&mut app, "Add", &[("text", Value::Int(3))]), Err(Refusal::Invalid(_))));
    // A wrong-sort id is the caller's mistake even when a guard would also fail.
    let user = id(&app, "User", 0);
    assert!(matches!(call(&mut app, "Lock", &[("n", user)]), Err(Refusal::Invalid(_))));
    assert!(matches!(call(&mut app, "Seed", &[]), Err(Refusal::Rejected(_))));
}

#[test]
fn guards_replay() {
    let mut app = notes();
    for t in ["a", "", "b", "c", "d"] {
        let _ = app.dispatch("Add", &[("text", text(t))]);
    }
    let n = app.ids("Note")[0].clone();
    app.dispatch("Lock", &[("n", n.clone())]).unwrap();
    let _ = app.dispatch("Lock", &[("n", n)]);
    let _ = app.dispatch("Seed", &[]);
    check_views(&app, "live").unwrap();
    check_base_invariant(&app.engine).unwrap();
    check_replay(NOTES, &app).unwrap();
    // Only the accepted events are in the log.
    let names: Vec<&str> = app.engine.log().iter().map(|e| e.name.as_str()).filter(|n| !n.starts_with('@')).collect();
    assert_eq!(names, ["Seed", "Add", "Add", "Add", "Lock"]);
}

// --- inline `if` ------------------------------------------------------------------------

const TOGGLE: &str = r#"
entity User { name: Text }
entity Msg { text: Text }
entity Like { msg: Msg, user: User }
entity Audit { what: Text }
state current : User
state count : Int = 0

event Seed(u: User)
event Bump(d: Int)
event Classify(m: Msg, n: Int)
event Post(text: Text)
event Twice()
event Outer(n: Int)

let u0 = new User { name: "Ada" }
let m0 = new Msg { text: "hi" }

on Seed(u) => set current = u
// The event is accepted either way; the condition picks what it does.
on Bump(d) {
  if (d > 0) { set count = count + d } else { new Audit { what: "refused a non-positive bump" } }
}
on Classify(m, n) {
  if (n < 0) { m.text := "negative" }
  else if (n = 0) { m.text := "zero" }
  else {
    if (n > 100) { m.text := "huge" } else { m.text := "positive" }
  }
}
// A row created in a branch is the branch's own.
on Post(text) {
  if (current) {
    let m = new Msg { text: text }
    new Like { msg: m, user: current }
  } else {
    let a = new Audit { what: text }
    a.what := "anonymous: " ++ text
  }
  new Audit { what: "posted" }
}
// Both conditions read the state before the event: both fire.
on Twice() {
  if (count = 0) { set count = count + 1 }
  if (count = 0) { new Audit { what: "still zero, as far as this event can see" } }
}
on Outer(n) {
  if (n > 0) { do Bump(n) } else { do Twice() }
}
let audits : Unit -> Int = count(Audit by unit)
"#;

fn count_of(app: &App) -> Value {
    app.field("State#", "count")[0].1.clone()
}

fn audits(app: &App) -> Vec<String> {
    app.field("Audit", "what").into_iter().map(|(_, v)| v.to_string()).collect()
}

#[test]
fn an_if_picks_a_branch_and_the_event_is_accepted_either_way() {
    let mut app = App::build(TOGGLE);
    let cursor = app.engine.cursor();
    app.dispatch("Bump", &[("d", Value::Int(3))]).unwrap();
    app.dispatch("Bump", &[("d", Value::Int(-1))]).unwrap();
    assert_eq!(count_of(&app), Value::Int(3));
    assert_eq!(audits(&app), ["\"refused a non-positive bump\""]);
    assert_eq!(app.engine.cursor(), cursor + 2, "both were logged");
}

#[test]
fn else_if_chains_and_nested_ifs() {
    let mut app = App::build(TOGGLE);
    let m = app.values["m0"].clone();
    for (n, want) in [(-5, "negative"), (0, "zero"), (7, "positive"), (101, "huge")] {
        app.dispatch("Classify", &[("m", m.clone()), ("n", Value::Int(n))]).unwrap();
        assert_eq!(app.field("Msg", "text")[0].1, text(want), "n = {n}");
    }
}

#[test]
fn a_row_named_in_a_branch_is_that_branchs_own() {
    let mut app = App::build(TOGGLE);
    // No current user: the else branch, which writes to the row it made.
    app.dispatch("Post", &[("text", text("a"))]).unwrap();
    assert_eq!(audits(&app), ["\"anonymous: a\"", "\"posted\""]);
    let u0 = app.values["u0"].clone();
    app.dispatch("Seed", &[("u", u0)]).unwrap();
    let (ids, _) = app.dispatch("Post", &[("text", text("b"))]).unwrap();
    // A message, a like naming it, and the audit row after the `if`.
    assert_eq!(ids.len(), 3);
    assert_eq!(app.field("Like", "msg")[0].1, ids[0]);
    check_base_invariant(&app.engine).unwrap();
    check_views(&app, "after Post").unwrap();
}

#[test]
fn conditions_in_one_handler_all_read_the_state_before_the_event() {
    let mut app = App::build(TOGGLE);
    app.dispatch("Twice", &[]).unwrap();
    assert_eq!(count_of(&app), Value::Int(1));
    assert_eq!(audits(&app).len(), 1);
    // Now the count is 1 for both.
    app.dispatch("Twice", &[]).unwrap();
    assert_eq!((count_of(&app), audits(&app).len()), (Value::Int(1), 1));
}

#[test]
fn a_do_inside_a_branch_runs_only_when_the_branch_does() {
    let mut app = App::build(TOGGLE);
    app.dispatch("Outer", &[("n", Value::Int(5))]).unwrap();
    assert_eq!((count_of(&app), audits(&app).len()), (Value::Int(5), 0));
    app.dispatch("Outer", &[("n", Value::Int(0))]).unwrap();
    // `Twice` saw count = 5: neither of its branches ran.
    assert_eq!((count_of(&app), audits(&app).len()), (Value::Int(5), 0));
    check_replay(TOGGLE, &app).unwrap();
}

// --- what is not allowed ------------------------------------------------------------------

fn error(body: &str) -> String {
    let src = format!("entity T {{ x: Int, b: Bool }}\nentity U {{ t: T }}\nstate s : Int = 0\nevent E(t: T, n: Int)\nevent F(t: T)\nevent G(t: T)\n{body}\n");
    common::check(&src).err().unwrap_or_else(|| panic!("accepted:\n{body}"))
}

#[test]
fn what_a_condition_may_not_be() {
    // It has no row of its own.
    let e = error("on E(t, n) where (.x > 0) => t.x := n");
    assert!(e.contains("has no row of its own"), "{e}");
    // The parentheses are part of the syntax, as on a view's `if`.
    let e = error("on E(t, n) where t.x > 0 => t.x := n");
    assert!(e.contains("where (condition)"), "{e}");
    let e = error("on E(t, n) where (t.x > 0) else oops => t.x := n");
    assert!(e.contains("expected a string after `else`"), "{e}");
    // A value is not a condition.
    let e = error("on E(t, n) where (t.x + n) => t.x := n");
    assert!(e.contains("must be a comparison or a `Bool`"), "{e}");
    // It cannot depend on a row the handler has just created.
    let e = error("on E(t, n) {\n  let u = new U { t: t }\n  if (u.t = t) { t.x := n }\n}");
    assert!(e.contains("was created by this handler"), "{e}");
    // A name bound in a branch does not outlive it.
    let e = error("on E(t, n) {\n  if (n > 0) { let u = new U { t: t } }\n  delete u\n}");
    assert!(e.contains("unknown parameter"), "{e}");
    // A `do` cycle is a cycle through a branch too.
    let e = error("on F(t) { if (t.b) { do G(t) } }\non G(t) => do F(t)");
    assert!(e.contains("cycle"), "{e}");
    // A type error inside a condition is reported at the condition.
    let e = error("on E(t, n) where (t.nope > 0) => t.x := n");
    assert!(e.contains("nope"), "{e}");
    let e = error("on E(t, n) where (count(T by unit) = \"three\") => t.x := n");
    assert!(e.contains("compare") || e.contains("Text"), "{e}");
}

#[test]
fn a_dom_handler_does_not_branch() {
    let e = common::check("entity T { x: Int }\nevent E(t: T)\nview main = ul { T as t select li(on click { if (t.x > 0) { do E(t) } }) { .x } }\n")
        .expect_err("an `if` in a DOM handler");
    assert!(e.contains("may not mutate directly") || e.contains("DOM handler"), "{e}");
}

#[test]
fn guards_and_ifs_print_and_parse() {
    let src = "entity T { x: Int }\nevent E(t: T)\non E(t) where (t.x > 0) else \"no\" {\n  if (t.x = 1) { delete t } else { t.x := 1 }\n}\n";
    let parsed = rex::parse(src);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let printed = rex::program_to_sexpr(&parsed.program);
    assert!(printed.contains("(where ") && printed.contains("\"no\""), "{printed}");
    assert!(printed.contains("(if "), "{printed}");
}

// --- conditions at the Unit level, which guards rely on ---------------------------------------

#[test]
fn a_bool_state_as_a_root_condition_means_its_value() {
    // It used to mean "has a value", so a `False` state showed its content.
    let src = "state open : Bool = False\nevent Toggle()\non Toggle() => set open = not open\nlet n : Unit -> Int = count(State by unit)\nview main = div { if (open) { p \"open\" } if (not open) { p \"closed\" } }\n".replace("let n : Unit -> Int = count(State by unit)\n", "");
    let mut app = App::build(&src);
    let shown = |app: &App| (app.view("main#unit#if1").len(), app.view("main#unit#if2").len());
    assert_eq!(shown(&app), (0, 1));
    app.dispatch("Toggle", &[]).unwrap();
    assert_eq!(shown(&app), (1, 0));
    check_views(&app, "after toggle").unwrap();
}

// --- a model: accounts with preconditions ---------------------------------------------------------

const BANK: &str = r#"
entity Acct { bal: Int, open: Bool }
state moved : Int = 0

event Open()
event Deposit(a: Acct, n: Int)
event Withdraw(a: Acct, n: Int)
event Transfer(a: Acct, b: Acct, n: Int)
event Close(a: Acct)
event Sweep(a: Acct, cap: Int)

on Open() => new Acct { bal: 0, open: True }
on Deposit(a, n)  where (a.open & n > 0) else "cannot deposit" => a.bal := a.bal + n
on Withdraw(a, n) where (a.open & n > 0 & a.bal >= n) else "cannot withdraw" => a.bal := a.bal - n
// Both guards must hold, against the state before the transfer.
on Transfer(a, b, n) where (a != b) else "same account" {
  do Withdraw(a, n)
  do Deposit(b, n)
  set moved = moved + n
}
on Close(a) where (a.open & a.bal = 0) else "cannot close" => a.open := False
on Sweep(a, cap) {
  if (a.open & a.bal > cap) {
    a.bal := cap
    set moved = moved + a.bal - cap
  } else if (a.open) {
    set moved = moved + 1
  }
}
"#;

#[derive(Clone, Debug)]
enum BankOp {
    Open,
    Deposit(u64, i64),
    Withdraw(u64, i64),
    Transfer(u64, u64, i64),
    Close(u64),
    Sweep(u64, i64),
}

fn bank_op() -> impl Strategy<Value = BankOp> {
    let (a, n) = (|| 0u64..5, || -2i64..9);
    prop_oneof![
        2 => Just(BankOp::Open),
        4 => (a(), n()).prop_map(|(a, n)| BankOp::Deposit(a, n)),
        3 => (a(), n()).prop_map(|(a, n)| BankOp::Withdraw(a, n)),
        3 => (a(), a(), n()).prop_map(|(a, b, n)| BankOp::Transfer(a, b, n)),
        2 => a().prop_map(BankOp::Close),
        2 => (a(), n()).prop_map(|(a, c)| BankOp::Sweep(a, c)),
    ]
}

#[derive(Default)]
struct Bank {
    /// `(balance, open)`, by account number.
    accts: Vec<(i64, bool)>,
    moved: i64,
}

impl Bank {
    fn open(&self, a: u64) -> bool {
        self.accts.get(a as usize).is_some_and(|x| x.1)
    }
    fn bal(&self, a: u64) -> i64 {
        self.accts.get(a as usize).map_or(0, |x| x.0)
    }

    /// Apply `op`; `Err(reason)` if the app must reject it.
    fn apply(&mut self, op: &BankOp) -> Result<(), &'static str> {
        match *op {
            BankOp::Open => self.accts.push((0, true)),
            BankOp::Deposit(a, n) => {
                if !(self.open(a) && n > 0) {
                    return Err("cannot deposit");
                }
                self.accts[a as usize].0 += n;
            }
            BankOp::Withdraw(a, n) => {
                if !(self.open(a) && n > 0 && self.bal(a) >= n) {
                    return Err("cannot withdraw");
                }
                self.accts[a as usize].0 -= n;
            }
            BankOp::Transfer(a, b, n) => {
                if a == b {
                    return Err("same account");
                }
                // Both callee guards read the state before the transfer.
                if !(self.open(a) && n > 0 && self.bal(a) >= n) {
                    return Err("cannot withdraw");
                }
                if !(self.open(b) && n > 0) {
                    return Err("cannot deposit");
                }
                self.accts[a as usize].0 -= n;
                self.accts[b as usize].0 += n;
                self.moved += n;
            }
            BankOp::Close(a) => {
                if !(self.open(a) && self.bal(a) == 0) {
                    return Err("cannot close");
                }
                self.accts[a as usize].1 = false;
            }
            // Never rejected: an account that is closed or missing is a no-op.
            BankOp::Sweep(a, cap) => {
                if self.open(a) && self.bal(a) > cap {
                    self.moved += self.bal(a) - cap;
                    self.accts[a as usize].0 = cap;
                } else if self.open(a) {
                    self.moved += 1;
                }
            }
        }
        Ok(())
    }
}

fn bank_dispatch(app: &mut App, op: &BankOp) -> Result<(), Refusal> {
    let acct = |app: &App, n: u64| ArgValue::Value(id(app, "Acct", n));
    let int = |n: i64| ArgValue::Value(Value::Int(n));
    let (name, args): (&str, Vec<(&str, ArgValue)>) = match *op {
        BankOp::Open => ("Open", vec![]),
        BankOp::Deposit(a, n) => ("Deposit", vec![("a", acct(app, a)), ("n", int(n))]),
        BankOp::Withdraw(a, n) => ("Withdraw", vec![("a", acct(app, a)), ("n", int(n))]),
        BankOp::Transfer(a, b, n) => ("Transfer", vec![("a", acct(app, a)), ("b", acct(app, b)), ("n", int(n))]),
        BankOp::Close(a) => ("Close", vec![("a", acct(app, a))]),
        BankOp::Sweep(a, cap) => ("Sweep", vec![("a", acct(app, a)), ("cap", int(cap))]),
    };
    let args = args.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    app.try_dispatch(name, &args).map(|_| ())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: common::cases(200), ..ProptestConfig::default() })]

    #[test]
    fn a_bank_with_preconditions_matches_its_model(ops in prop::collection::vec(bank_op(), 0..40)) {
        let mut app = App::build(BANK);
        let mut model = Bank::default();
        for (i, op) in ops.iter().enumerate() {
            let before = fingerprint(&app);
            let want = model.apply(op);
            let got = bank_dispatch(&mut app, op);
            match (&want, &got) {
                (Ok(()), Ok(())) => {}
                // Rejected for the model's reason, and nothing moved.
                (Err(reason), Err(Refusal::Rejected(said))) => {
                    prop_assert_eq!(said, reason, "step {} {:?}", i, op);
                    prop_assert_eq!(fingerprint(&app), before.clone(), "step {} {:?}: a rejection changed something", i, op);
                }
                _ => prop_assert!(false, "step {} {:?}: model {:?}, app {:?}", i, op, want, got),
            }
            let bal: Vec<Value> = app.field("Acct", "bal").into_iter().map(|(_, v)| v).collect();
            let open: Vec<Value> = app.field("Acct", "open").into_iter().map(|(_, v)| v).collect();
            prop_assert_eq!(bal, model.accts.iter().map(|a| Value::Int(a.0)).collect::<Vec<_>>(), "balances after step {} {:?}", i, op);
            prop_assert_eq!(open, model.accts.iter().map(|a| Value::atom(if a.1 { "True" } else { "False" })).collect::<Vec<_>>(), "open after step {}", i);
            prop_assert_eq!(app.field("State#", "moved")[0].1.clone(), Value::Int(model.moved), "moved after step {} {:?}", i, op);
            check_base_invariant(&app.engine).map_err(TestCaseError::fail)?;
        }
        check_views(&app, "at the end").map_err(TestCaseError::fail)?;
        check_replay(BANK, &app).map_err(TestCaseError::fail)?;
    }
}

// --- what guards made natural to write, and so had to work ------------------------------------------

const LIKES: &str = r#"
type Mood = Happy | Sad
entity User { name: Text, mood: Mood }
entity Msg { text: Text, pinned: Bool }
entity Like { msg: Msg, user: User, strong: Bool }
state current : User

event Login(u: User)
event Toggle(m: Msg)
event Unlike(m: Msg, strong: Bool)
event Feel(u: User, mood: Mood)

let u0 = new User { name: "Ada", mood: Happy }
let u1 = new User { name: "Bo", mood: Sad }
let m0 = new Msg { text: "a", pinned: True }
let m1 = new Msg { text: "b", pinned: False }

// A message's like by the current user, if any.
let mine : Msg -> Like = ~((Like where .user = current) . .msg)
// Conjuncts of different types: a state that is set, a relation that is
// absent, a `Bool` field. Each is a condition; all must hold.
let likeable : Msg -> Msg = Msg where (current & not mine & .pinned)
// Conjuncts of the same type are one relation, intersected: the sender-style
// test "this like's user IS the current user".
let my_likes : Like -> User = Like . (.user & current)

on Login(u) => set current = u
on Toggle(m) where (current & m) {
  // Two conditions on one target, with and without parentheses.
  if (m.mine) { delete Like where .msg = m & .user = current }
  else        { new Like { msg: m, user: current, strong: m.pinned } }
}
on Unlike(m, strong) => delete Like where (.msg = m & .strong = strong & .user = current)
// An argument against a constructor of its type.
on Feel(u, mood) where (mood != Sad | u.mood = Happy) else "no" => u.mood := mood
"#;

#[test]
fn a_toggle_keeps_one_like_per_user_per_message() {
    let mut app = App::build(LIKES);
    let (u0, u1, m0, m1) = (app.values["u0"].clone(), app.values["u1"].clone(), app.values["m0"].clone(), app.values["m1"].clone());
    let likes = |app: &App| app.field("Like", "user").len();
    app.rejected("Toggle", &[("m", m0.clone())]);
    app.dispatch("Login", &[("u", u0.clone())]).unwrap();
    for (m, want) in [(&m0, 1), (&m1, 2), (&m0, 1), (&m0, 2), (&m1, 1)] {
        app.dispatch("Toggle", &[("m", m.clone())]).unwrap();
        assert_eq!(likes(&app), want);
        check_views(&app, "after a toggle").unwrap();
    }
    // Another user's like of the same message is theirs: untouched by Ada's.
    app.dispatch("Login", &[("u", u1)]).unwrap();
    app.dispatch("Toggle", &[("m", m0.clone())]).unwrap();
    assert_eq!(likes(&app), 2);
    app.dispatch("Login", &[("u", u0)]).unwrap();
    app.dispatch("Toggle", &[("m", m0.clone())]).unwrap();
    assert_eq!(likes(&app), 1);
    // Three conditions, one of them an argument: only a matching row goes.
    app.dispatch("Unlike", &[("m", m0.clone()), ("strong", Value::atom("False"))]).unwrap();
    assert_eq!(likes(&app), 1);
    check_replay(LIKES, &app).unwrap();
}

#[test]
fn conjuncts_of_different_types_are_each_a_condition() {
    let mut app = App::build(LIKES);
    let (u0, m0) = (app.values["u0"].clone(), app.values["m0"].clone());
    let keys = |app: &App, view: &str| app.view(view).into_iter().map(|(k, _, _)| k).collect::<Vec<_>>();
    // Nobody is logged in: `current` fails for every row.
    assert!(keys(&app, "likeable").is_empty());
    app.dispatch("Login", &[("u", u0.clone())]).unwrap();
    // Logged in, not liked — and only m0 is pinned.
    assert_eq!(keys(&app, "likeable"), std::slice::from_ref(&m0));
    let (ids, _) = app.dispatch("Toggle", &[("m", m0.clone())]).unwrap();
    assert!(keys(&app, "likeable").is_empty());
    // Same-typed conjuncts intersect: the like's user is the current user.
    assert_eq!(app.view("my_likes"), [(ids[0].clone(), u0, 1)]);
    app.dispatch("Login", &[("u", app.values["u1"].clone())]).unwrap();
    assert!(app.view("my_likes").is_empty());
    assert_eq!(keys(&app, "likeable"), [m0]);
    check_views(&app, "at the end").unwrap();
}

#[test]
fn an_argument_compares_with_a_constructor_of_its_type() {
    let mut app = App::build(LIKES);
    let (u0, u1) = (app.values["u0"].clone(), app.values["u1"].clone());
    // Sad is allowed only for a user who is Happy.
    app.dispatch("Feel", &[("u", u0.clone()), ("mood", Value::atom("Sad"))]).unwrap();
    assert_eq!(app.rejected("Feel", &[("u", u1.clone()), ("mood", Value::atom("Sad"))]), "no");
    app.dispatch("Feel", &[("u", u1), ("mood", Value::atom("Happy"))]).unwrap();
    // A constructor of another type is still a type error.
    let e = common::check(&LIKES.replace("mood != Sad", "mood != True")).unwrap_err();
    assert!(e.contains("cannot compare"), "{e}");
}

// --- `reject`: what a guard is sugar for -----------------------------------------------------

const REJECTS: &str = r#"
entity User { name: Text }
entity Note { text: Text, owner: User }
entity Audit { what: Text }
state current : User
state n : Int = 0

event Login(u: User)
event Add(text: Text)
event Late(text: Text)
event Outer(text: Text)
event Off()
event Edit(x: Note, text: Text)
event EditG(x: Note, text: Text)

let u0 = new User { name: "Ada" }

on Login(u) => set current = u
// Several preconditions, each with its own reason.
on Add(text) {
  if (not current)      { reject "pick a user" }
  else if (text = "")   { reject "type something" }
  else if (n >= 2)      { reject }
  new Note { text: text, owner: current }
  set n = n + 1
}
// A reject after writes: the writes are not kept.
on Late(text) {
  new Audit { what: text }
  let a = new Audit { what: "second" }
  set n = n + 100
  if (text = "bad") { reject "late" }
}
// A callee's reject is the caller's.
on Outer(text) {
  new Audit { what: "outer" }
  do Late(text)
}
on Off() { reject "disabled" }
// The same precondition, as an `if` and as a guard.
on Edit(x, text)  { if (x.owner = current) { x.text := text } else { reject "not yours" } }
on EditG(x, text) where (x.owner = current) else "not yours" => x.text := text
"#;

#[test]
fn reject_gives_each_precondition_its_own_reason() {
    let mut app = App::build(REJECTS);
    assert_eq!(app.rejected("Add", &[("text", text("a"))]), "pick a user");
    app.dispatch("Login", &[("u", app.values["u0"].clone())]).unwrap();
    assert_eq!(app.rejected("Add", &[("text", text(""))]), "type something");
    app.dispatch("Add", &[("text", text("a"))]).unwrap();
    app.dispatch("Add", &[("text", text("b"))]).unwrap();
    // No reason given: one naming the event.
    assert_eq!(app.rejected("Add", &[("text", text("c"))]), "`Add` was rejected");
    assert_eq!(app.ids("Note").len(), 2);
    assert_eq!(app.rejected("Off", &[]), "disabled");
}

#[test]
fn a_reject_after_writes_keeps_none_of_them() {
    let mut app = App::build(REJECTS);
    let before = fingerprint(&app);
    assert_eq!(app.rejected("Late", &[("text", text("bad"))]), "late");
    // …nor the caller's, when the reject is in a handler it `do`es.
    assert_eq!(app.rejected("Outer", &[("text", text("bad"))]), "late");
    assert_eq!(fingerprint(&app), before);
    // The ids the rejected events would have minted are still free.
    let (ids, _) = app.dispatch("Outer", &[("text", text("ok"))]).unwrap();
    assert_eq!(ids, [id(&app, "Audit", 0), id(&app, "Audit", 1), id(&app, "Audit", 2)]);
    assert_eq!(app.field("State#", "n")[0].1, Value::Int(100));
    check_views(&app, "after").unwrap();
    check_replay(REJECTS, &app).unwrap();
}

#[test]
fn a_guard_is_an_if_whose_else_rejects() {
    // Same IR…
    let app = App::build(REJECTS);
    // (up to the generated name of the condition's hidden view)
    let shape = |event: &str| format!("{:?}", app.event(event).body).replace(&format!("on#{event}#"), "on#_#");
    assert_eq!(shape("Edit").replace("#3", "#"), shape("EditG").replace("#4", "#"));
    // …so the same outcomes: on a row that is theirs, one that is not, and
    // one that does not exist (where `if (not c) { reject }` would differ).
    let mut app = app;
    let u0 = app.values["u0"].clone();
    app.dispatch("Login", &[("u", u0)]).unwrap();
    let (ids, _) = app.dispatch("Add", &[("text", text("a"))]).unwrap();
    for event in ["Edit", "EditG"] {
        app.dispatch(event, &[("x", ids[0].clone()), ("text", text(event))]).unwrap();
        assert_eq!(app.rejected(event, &[("x", id(&app, "Note", 9)), ("text", text("z"))]), "not yours");
    }
    assert_eq!(app.field("Note", "text")[0].1, text("EditG"));
}

#[test]
fn what_reject_may_not_be() {
    let e = error("on E(t, n) {\n  reject \"no\"\n  t.x := n\n}");
    assert!(e.contains("can never run"), "{e}");
    // In a branch it ends only that path; what follows the `if` is reachable.
    common::check("entity T { x: Int }\nevent E(t: T, n: Int)\non E(t, n) {\n  if (n < 0) { reject \"no\" }\n  t.x := n\n}\n").unwrap();
    let e = common::check("entity T { x: Int }\nview main = ul { T as t select li(on click { reject \"no\" }) { .x } }\n")
        .expect_err("a `reject` in a DOM handler");
    assert!(e.contains("DOM handler"), "{e}");
    // `reject` is a verb only at the start of a statement, not a reserved word.
    common::check("entity T { reject: Int }\nevent E(t: T, reject: Int)\non E(t, reject) => t.reject := reject\n").unwrap();
    let parsed = rex::parse("entity T { x: Int }\nevent E(t: T)\non E(t) { if (t.x > 0) { reject \"no\" } else { reject } }\n");
    assert!(parsed.diagnostics.is_empty());
    let printed = rex::program_to_sexpr(&parsed.program);
    assert!(printed.contains("(reject \"no\")") && printed.contains("(reject)"), "{printed}");
}

// --- row tests: a condition that asks whether a target has any rows ---------------------------

const ROWS: &str = r#"
entity User { name: Text }
entity Tag { name: Text }
entity Has { user: User, tag: Tag, pinned: Bool }
state current : User
state n : Int = 0

event Login(u: User)
event Add(u: User, t: Tag)
event AddOnce(u: User, t: Tag)
event Drop(u: User, t: Tag)
event Pin(u: User, t: Tag)
event Count()
event Mine(t: Tag)

let u0 = new User { name: "Ada" }
let u1 = new User { name: "Bo" }
let t0 = new Tag { name: "x" }
let t1 = new Tag { name: "y" }

on Login(u) => set current = u
// Two params: is there such a row?
on Add(u, t) {
  if (Has where .user = u & .tag = t) { reject "already tagged" }
  new Has { user: u, tag: t, pinned: False }
}
// The same, negated, as a guard.
on AddOnce(u, t) where (not (Has where .user = u & .tag = t)) else "already tagged" =>
  new Has { user: u, tag: t, pinned: False }
on Drop(u, t) where (Has where .user = u & .tag = t) else "not tagged" =>
  delete Has where .user = u & .tag = t
on Pin(u, t) => update Has where .user = u & .tag = t { pinned: True }
// No param at all: a row test over the whole entity.
on Count() {
  if (Has where .pinned) { set n = n + 1 }
  if (not (Has where .pinned)) { set n = n - 1 }
}
// Beside other conjuncts — in parentheses, so its `&` stays its own.
on Mine(t) where (current & (Has where .tag = t & .user = current) & n >= 0) else "not yours" => set n = n + 10
"#;

#[test]
fn a_condition_can_ask_whether_a_row_exists() {
    let mut app = App::build(ROWS);
    let (u0, u1, t0, t1) = (app.values["u0"].clone(), app.values["u1"].clone(), app.values["t0"].clone(), app.values["t1"].clone());
    let at = |u: &Value, t: &Value| [("u", u.clone()), ("t", t.clone())];
    let n = |app: &App| app.field("State#", "n")[0].1.clone();
    assert_eq!(app.rejected("Drop", &at(&u0, &t0)), "not tagged");
    app.dispatch("Add", &at(&u0, &t0)).unwrap();
    assert_eq!(app.rejected("Add", &at(&u0, &t0)), "already tagged");
    assert_eq!(app.rejected("AddOnce", &at(&u0, &t0)), "already tagged");
    // Either part of the pair differing is a different row.
    app.dispatch("AddOnce", &at(&u0, &t1)).unwrap();
    app.dispatch("Add", &at(&u1, &t0)).unwrap();
    assert_eq!(app.ids("Has").len(), 3);
    app.dispatch("Drop", &at(&u0, &t0)).unwrap();
    app.dispatch("AddOnce", &at(&u0, &t0)).unwrap();

    // With no param: nothing is pinned, then something is.
    app.dispatch("Count", &[]).unwrap();
    assert_eq!(n(&app), Value::Int(-1));
    app.dispatch("Pin", &at(&u1, &t0)).unwrap();
    app.dispatch("Count", &[]).unwrap();
    app.dispatch("Count", &[]).unwrap();
    assert_eq!(n(&app), Value::Int(1));

    // Among other conjuncts; `current` unset makes the row test's predicate
    // read nothing, which matches no row.
    assert_eq!(app.rejected("Mine", &[("t", t1.clone())]), "not yours");
    app.dispatch("Login", &[("u", u1)]).unwrap();
    assert_eq!(app.rejected("Mine", &[("t", t1.clone())]), "not yours");
    app.dispatch("Mine", &[("t", t0)]).unwrap();
    assert_eq!(n(&app), Value::Int(11));
    app.dispatch("Login", &[("u", u0)]).unwrap();
    app.dispatch("Mine", &[("t", t1)]).unwrap();
    check_views(&app, "at the end").unwrap();
    check_replay(ROWS, &app).unwrap();
}

#[test]
fn only_a_row_test_has_a_row_of_its_own() {
    // `.field` is the row under test inside one, and an error anywhere else.
    let e = error("on E(t, n) where (.x > 0 & (T where .x = n)) => t.x := n");
    assert!(e.contains("has no row of its own"), "{e}");
    let e = error("on E(t, n) where (T where .nope = n) => t.x := n");
    assert!(e.contains("nope"), "{e}");
    let e = error("on E(t, n) where (Nope where .x = n) => t.x := n");
    assert!(e.contains("Nope"), "{e}");
    common::check("entity T { x: Int, b: Bool }\nevent E(t: T, n: Int)\non E(t, n) where (not (T where .x = n & .b) & t.x != n) => t.x := n\n").unwrap();
}
