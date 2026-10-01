//! Total aggregates: a fold over an empty group is its initial value.
//!
//! Which groups exist whatever the data holds is decided by the *type* of the
//! group key: the one `Unit` point, the live rows of an entity, the
//! constructors of an enum. Over those, `count` and `sum` give `0` for an
//! empty group. `min`, `max` and `avg` have no initial value and stay absent;
//! a scalar key (`Int`, `Text`) has no domain to range over and stays partial.

mod common;

use common::{check_base_invariant, check_deltas, check_replay, check_restore, check_views, App};
use proptest::prelude::*;
use rex::eval::Value;
use std::collections::BTreeMap;

const SHOP: &str = r#"
type Status = Open | Paid | Void
entity List { title: Text }
entity Card { list: List, title: Text, points: Int, cost: Money, status: Status, done: Bool }

event AddList(title: Text)
event DropList(l: List)
event AddCard(l: List, title: Text, points: Int, status: Status)
event DropCard(c: Card)
event Move(c: Card, l: List)
event Finish(c: Card)
event AddIfRoom(l: List)
event Fill(title: Text)
event Purge(l: List)

// An entity key: every live list is a group.
let cards      : List -> Int   = count(Card by .list)
let points     : List -> Int   = sum((Card . .points) by .list)
let spent      : List -> Money = sum((Card . .cost) by .list)
let done_cards : List -> Int   = count((Card where .done) by .list)
// The same aggregate without `by`: the image is already keyed by the list.
let cards_inv  : List -> Int   = count(~(Card . .list))
// No initial value: absent for an empty group.
let biggest    : List -> Int   = max((Card . .points) by .list)
let smallest   : List -> Int   = min((Card . .points) by .list)
let mean       : List -> Money = avg((Card . .points) by .list)
// An enum key: every constructor is a group.
let by_status  : Status -> Int = count(Card by .status)
let by_done    : Bool -> Int   = count(Card by .done)
// A scalar key: groups exist only as the data produces them.
let by_title   : Text -> Int   = count(Card by .title)
let by_points  : Int -> Int    = count(Card by .points)
// Totals compose like any other relation.
let empty      : List -> List  = List where cards = 0
let roomy      : List -> List  = List where cards < 2
let load       : List -> Int   = cards + done_cards
let n_empty    : Unit -> Int   = count(empty by unit)

on AddList(title) => new List { title: title }
on DropList(l) => delete l
on AddCard(l, title, points, status) =>
  new Card { list: l, title: title, points: points, cost: 1.50, status: status, done: False }
on DropCard(c) => delete c
on Move(c, l) => c.list := l
on Finish(c) => c.done := True
// A key and its group arriving, and leaving, in one transaction.
on Fill(title) {
  let l = new List { title: title }
  new Card { list: l, title: "a", points: 2, cost: 1.00, status: Open, done: False }
  new Card { list: l, title: "b", points: 3, cost: 1.00, status: Open, done: False }
}
on Purge(l) {
  delete Card where .list = l
  delete l
}
on AddIfRoom(l) where (l.cards < 2) else "full" =>
  new Card { list: l, title: "x", points: 1, cost: 0.25, status: Open, done: False }
"#;

fn text(s: &str) -> Value {
    Value::text(s)
}

fn int(app: &App, view: &str) -> Vec<(Value, i64)> {
    app.view(view)
        .into_iter()
        .map(|(k, v, w)| {
            assert_eq!(w, 1, "`{view}` has a row of weight {w}");
            (k, v.as_i64().unwrap_or_else(|| panic!("`{view}` holds {v:?}")))
        })
        .collect()
}

/// Rows keyed by atoms, in the order values sort (not alphabetical).
fn atoms(rows: &[(&str, i64)]) -> Vec<(Value, i64)> {
    let mut v: Vec<(Value, i64)> = rows.iter().map(|(a, n)| (Value::atom(a), *n)).collect();
    v.sort();
    v
}

/// Dispatch, checking every view's delta against the change in its value and
/// the circuit against the batch oracle.
fn step(app: &mut App, name: &str, args: &[(&str, Value)]) -> Vec<Value> {
    let before = check_views(app, "before").unwrap();
    let (ids, res) = app.dispatch(name, args).unwrap_or_else(|e| panic!("{name}: {e}"));
    let after = check_views(app, &format!("after {name}")).unwrap();
    check_deltas(&res, &before, &after, name).unwrap();
    check_base_invariant(&app.engine).unwrap();
    ids
}

fn add_card(app: &mut App, l: &Value, title: &str, points: i64, status: &str) -> Value {
    step(app, "AddCard", &[("l", l.clone()), ("title", text(title)), ("points", Value::Int(points)), ("status", Value::atom(status))])
        .remove(0)
}

#[test]
fn an_empty_group_of_an_entity_key_counts_and_sums_to_zero() {
    let mut app = App::build(SHOP);
    // No lists: no keys, so no rows — a total is over the keys that exist.
    for view in ["cards", "points", "spent", "done_cards", "cards_inv", "empty"] {
        assert!(app.view(view).is_empty(), "{view}");
    }
    let a = step(&mut app, "AddList", &[("title", text("a"))]).remove(0);
    // The list is a group the moment it exists.
    assert_eq!(int(&app, "cards"), [(a.clone(), 0)]);
    assert_eq!(int(&app, "points"), [(a.clone(), 0)]);
    assert_eq!(int(&app, "done_cards"), [(a.clone(), 0)]);
    assert_eq!(int(&app, "cards_inv"), [(a.clone(), 0)]);
    // `sum` of `Money` is `Money`.
    assert_eq!(app.view("spent"), [(a.clone(), Value::Money(0), 1)]);
    // No initial value for these.
    for view in ["biggest", "smallest", "mean"] {
        assert!(app.view(view).is_empty(), "{view}");
    }

    let c = add_card(&mut app, &a, "one", 5, "Open");
    assert_eq!(int(&app, "cards"), [(a.clone(), 1)]);
    assert_eq!(int(&app, "points"), [(a.clone(), 5)]);
    assert_eq!(app.view("spent"), [(a.clone(), Value::Money(150), 1)]);
    assert_eq!(int(&app, "biggest"), [(a.clone(), 5)]);
    // A filtered image: nothing done yet, still a row.
    assert_eq!(int(&app, "done_cards"), [(a.clone(), 0)]);
    step(&mut app, "Finish", &[("c", c.clone())]);
    assert_eq!(int(&app, "done_cards"), [(a.clone(), 1)]);

    // Back to empty: back to zero, not to absent.
    step(&mut app, "DropCard", &[("c", c)]);
    assert_eq!(int(&app, "cards"), [(a.clone(), 0)]);
    assert_eq!(int(&app, "points"), [(a.clone(), 0)]);
    assert!(app.view("biggest").is_empty());
    // The group goes with its key.
    step(&mut app, "DropList", &[("l", a)]);
    assert!(app.view("cards").is_empty() && app.view("spent").is_empty());
}

#[test]
fn a_group_follows_its_rows_between_keys() {
    let mut app = App::build(SHOP);
    let a = step(&mut app, "AddList", &[("title", text("a"))]).remove(0);
    let b = step(&mut app, "AddList", &[("title", text("b"))]).remove(0);
    let c = add_card(&mut app, &a, "one", 3, "Open");
    assert_eq!(int(&app, "cards"), [(a.clone(), 1), (b.clone(), 0)]);
    step(&mut app, "Move", &[("c", c.clone()), ("l", b.clone())]);
    assert_eq!(int(&app, "cards"), [(a.clone(), 0), (b.clone(), 1)]);
    assert_eq!(int(&app, "points"), [(a.clone(), 0), (b.clone(), 3)]);

    // A list deleted with a card still pointing at it: the row is no longer a
    // key in its own right, but the group is not empty, so it keeps its count.
    step(&mut app, "DropList", &[("l", b.clone())]);
    assert_eq!(int(&app, "cards"), [(a.clone(), 0), (b.clone(), 1)]);
    // …until the card goes, and then nothing holds the key.
    step(&mut app, "DropCard", &[("c", c)]);
    assert_eq!(int(&app, "cards"), [(a, 0)]);
}

#[test]
fn a_key_and_its_group_can_arrive_and_leave_together() {
    let mut app = App::build(SHOP);
    let a = step(&mut app, "AddList", &[("title", text("a"))]).remove(0);
    // One step: the list and two cards. The delta is the count, with no 0 on the way.
    let before = check_views(&app, "before").unwrap();
    let (ids, res) = app.dispatch("Fill", &[("title", text("b"))]).unwrap();
    let b = ids[0].clone();
    assert_eq!(res.view_deltas["cards"].to_sorted_vec(), [(b.clone(), Value::Int(2), 1)]);
    assert_eq!(res.view_deltas["points"].to_sorted_vec(), [(b.clone(), Value::Int(5), 1)]);
    check_deltas(&res, &before, &check_views(&app, "after Fill").unwrap(), "Fill").unwrap();
    assert_eq!(int(&app, "cards"), [(a.clone(), 0), (b.clone(), 2)]);
    // And out again, in one step.
    let before = check_views(&app, "before").unwrap();
    let (_, res) = app.dispatch("Purge", &[("l", b.clone())]).unwrap();
    assert_eq!(res.view_deltas["cards"].to_sorted_vec(), [(b, Value::Int(2), -1)]);
    check_deltas(&res, &before, &check_views(&app, "after Purge").unwrap(), "Purge").unwrap();
    assert_eq!(int(&app, "cards"), [(a.clone(), 0)]);
    // An empty list leaves as a 0.
    let (_, res) = app.dispatch("Purge", &[("l", a.clone())]).unwrap();
    assert_eq!(res.view_deltas["cards"].to_sorted_vec(), [(a, Value::Int(0), -1)]);
    assert!(app.view("cards").is_empty());
}

#[test]
fn every_constructor_of_an_enum_key_is_a_group() {
    let mut app = App::build(SHOP);
    // There before any data: the constructors exist by construction.
    assert_eq!(int(&app, "by_status"), atoms(&[("Open", 0), ("Paid", 0), ("Void", 0)]));
    assert_eq!(int(&app, "by_done"), atoms(&[("False", 0), ("True", 0)]));
    let a = step(&mut app, "AddList", &[("title", text("a"))]).remove(0);
    let c = add_card(&mut app, &a, "one", 1, "Paid");
    add_card(&mut app, &a, "two", 1, "Paid");
    assert_eq!(int(&app, "by_status"), atoms(&[("Open", 0), ("Paid", 2), ("Void", 0)]));
    step(&mut app, "Finish", &[("c", c)]);
    assert_eq!(int(&app, "by_done"), atoms(&[("False", 1), ("True", 1)]));
}

#[test]
fn a_scalar_key_has_no_domain_and_stays_partial() {
    let mut app = App::build(SHOP);
    assert!(app.view("by_title").is_empty() && app.view("by_points").is_empty());
    let a = step(&mut app, "AddList", &[("title", text("a"))]).remove(0);
    let c = add_card(&mut app, &a, "one", 7, "Open");
    assert_eq!(int(&app, "by_title"), [(text("one"), 1)]);
    assert_eq!(int(&app, "by_points"), [(Value::Int(7), 1)]);
    step(&mut app, "DropCard", &[("c", c)]);
    // No card is titled "one" any more, and nothing else makes "one" a key.
    assert!(app.view("by_title").is_empty() && app.view("by_points").is_empty());
}

#[test]
fn a_total_count_compares_and_adds_like_a_number() {
    let mut app = App::build(SHOP);
    let a = step(&mut app, "AddList", &[("title", text("a"))]).remove(0);
    let b = step(&mut app, "AddList", &[("title", text("b"))]).remove(0);
    let keys = |app: &App, view: &str| app.view(view).into_iter().map(|(k, _, _)| k).collect::<Vec<_>>();
    // "Lists with no cards" is `cards = 0`, and "fewer than two" includes none.
    assert_eq!(keys(&app, "empty"), [a.clone(), b.clone()]);
    assert_eq!(keys(&app, "roomy"), [a.clone(), b.clone()]);
    assert_eq!(int(&app, "n_empty"), [(Value::Unit, 2)]);
    // Arithmetic of two totals has a row for every key.
    assert_eq!(int(&app, "load"), [(a.clone(), 0), (b.clone(), 0)]);
    add_card(&mut app, &a, "one", 1, "Open");
    add_card(&mut app, &a, "two", 1, "Open");
    assert_eq!(keys(&app, "empty"), std::slice::from_ref(&b));
    assert_eq!(keys(&app, "roomy"), std::slice::from_ref(&b));
    assert_eq!(int(&app, "n_empty"), [(Value::Unit, 1)]);
    // A guard on a count admits the first row of a group.
    assert_eq!(app.rejected("AddIfRoom", &[("l", a)]), "full");
    step(&mut app, "AddIfRoom", &[("l", b.clone())]);
    step(&mut app, "AddIfRoom", &[("l", b.clone())]);
    assert_eq!(app.rejected("AddIfRoom", &[("l", b)]), "full");
    assert_eq!(int(&app, "n_empty"), [(Value::Unit, 0)]);
}

#[test]
fn totals_survive_replay_and_restore() {
    let mut app = App::build(SHOP);
    let a = step(&mut app, "AddList", &[("title", text("a"))]).remove(0);
    step(&mut app, "AddList", &[("title", text("b"))]);
    add_card(&mut app, &a, "one", 2, "Void");
    let snap = app.engine.base_snapshot();
    step(&mut app, "AddList", &[("title", text("c"))]);
    check_replay(SHOP, &app).unwrap();
    check_restore(SHOP, &app, &snap).unwrap();
}

// --- a model -----------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    AddList,
    DropList(usize),
    AddCard(usize, i64, usize),
    DropCard(usize),
    Move(usize, usize),
    Finish(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => Just(Op::AddList),
        1 => (0usize..6).prop_map(Op::DropList),
        4 => (0usize..6, -3i64..9, 0usize..3).prop_map(|(l, p, s)| Op::AddCard(l, p, s)),
        2 => (0usize..12).prop_map(Op::DropCard),
        2 => (0usize..12, 0usize..6).prop_map(|(c, l)| Op::Move(c, l)),
        1 => (0usize..12).prop_map(Op::Finish),
    ]
}

const STATUSES: [&str; 3] = ["Open", "Paid", "Void"];

#[derive(Default)]
struct Model {
    lists: Vec<Value>,
    /// card → (list, points, status, done)
    cards: BTreeMap<Value, (Value, i64, usize, bool)>,
}

impl Model {
    /// `count` and `sum` by list: a row per live list, and per list a card
    /// still points at.
    fn by_list(&self, pick: impl Fn(&(Value, i64, usize, bool)) -> Option<i64>) -> Vec<(Value, i64)> {
        let mut out: BTreeMap<Value, i64> = self.lists.iter().map(|l| (l.clone(), 0)).collect();
        for card in self.cards.values() {
            if let Some(n) = pick(card) {
                *out.entry(card.0.clone()).or_default() += n;
            }
        }
        out.into_iter().collect()
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: common::cases(150), ..ProptestConfig::default() })]

    #[test]
    fn totals_match_a_model(ops in prop::collection::vec(op(), 0..40)) {
        let mut app = App::build(SHOP);
        let mut m = Model::default();
        for op in &ops {
            let pick = |v: &[Value], i: usize| if v.is_empty() { None } else { Some(v[i % v.len()].clone()) };
            let cards: Vec<Value> = m.cards.keys().cloned().collect();
            match op {
                Op::AddList => m.lists.push(step(&mut app, "AddList", &[("title", text("l"))]).remove(0)),
                Op::DropList(i) => {
                    if let Some(l) = pick(&m.lists, *i) {
                        step(&mut app, "DropList", &[("l", l.clone())]);
                        m.lists.retain(|x| *x != l);
                    }
                }
                Op::AddCard(i, points, s) => {
                    if let Some(l) = pick(&m.lists, *i) {
                        let c = add_card(&mut app, &l, "t", *points, STATUSES[*s]);
                        m.cards.insert(c, (l, *points, *s, false));
                    }
                }
                Op::DropCard(i) => {
                    if let Some(c) = pick(&cards, *i) {
                        step(&mut app, "DropCard", &[("c", c.clone())]);
                        m.cards.remove(&c);
                    }
                }
                Op::Move(i, j) => {
                    if let (Some(c), Some(l)) = (pick(&cards, *i), pick(&m.lists, *j)) {
                        step(&mut app, "Move", &[("c", c.clone()), ("l", l.clone())]);
                        m.cards.get_mut(&c).unwrap().0 = l;
                    }
                }
                Op::Finish(i) => {
                    if let Some(c) = pick(&cards, *i) {
                        step(&mut app, "Finish", &[("c", c.clone())]);
                        m.cards.get_mut(&c).unwrap().3 = true;
                    }
                }
            }
            prop_assert_eq!(int(&app, "cards"), m.by_list(|_| Some(1)), "cards after {:?}", op);
            prop_assert_eq!(int(&app, "cards_inv"), m.by_list(|_| Some(1)), "cards_inv after {:?}", op);
            prop_assert_eq!(int(&app, "points"), m.by_list(|c| Some(c.1)), "points after {:?}", op);
            prop_assert_eq!(int(&app, "done_cards"), m.by_list(|c| c.3.then_some(1)), "done_cards after {:?}", op);
            // `max` is there exactly for the lists that have a card.
            let with_cards: Vec<Value> = m.by_list(|_| Some(1)).into_iter().filter(|(_, n)| *n > 0).map(|(l, _)| l).collect();
            prop_assert_eq!(app.view("biggest").into_iter().map(|(k, _, _)| k).collect::<Vec<_>>(), with_cards, "biggest after {:?}", op);
            let status: Vec<(Value, i64)> = {
                let mut v: Vec<(Value, i64)> = STATUSES
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (Value::atom(s), m.cards.values().filter(|c| c.2 == i).count() as i64))
                    .collect();
                v.sort();
                v
            };
            prop_assert_eq!(int(&app, "by_status"), status, "by_status after {:?}", op);
        }
        check_replay(SHOP, &app).map_err(TestCaseError::fail)?;
    }
}
